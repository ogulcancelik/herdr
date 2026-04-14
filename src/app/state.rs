use crate::config::{Keybinds, SoundConfig, ToastConfig};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;

use crate::layout::{PaneInfo, SplitBorder};
use crate::selection::Selection;
use crate::terminal_theme::TerminalTheme;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Theme palette — all UI colors in one place, ready for theming
// ---------------------------------------------------------------------------

/// All colors used by the UI. Derived from a base accent color for now,
/// but structured so a full theme system can replace it later.
#[derive(Clone)]
#[allow(dead_code)] // all fields defined for theming — some used later
pub struct Palette {
    /// Primary accent (highlight, active borders).
    pub accent: Color,
    /// Background for floating panels, overlays, and modals.
    pub panel_bg: Color,
    /// Subtle surface background for selected/focused items.
    pub surface0: Color,
    /// Slightly lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text (secondary info, numbers).
    pub overlay0: Color,
    /// Slightly brighter overlay text.
    pub overlay1: Color,
    /// Main text color — soft white.
    pub text: Color,
    /// Subdued text (workspace numbers, dim labels).
    pub subtext0: Color,
    /// Branch name / special label color.
    pub mauve: Color,
    /// Done / idle states.
    pub green: Color,
    /// Working / running states.
    pub yellow: Color,
    /// Needs attention / blocked states.
    pub red: Color,
    /// Unseen / done notification accent.
    pub blue: Color,
    /// Notification accent / unseen markers.
    pub teal: Color,
    /// Interrupted / warning states.
    pub peach: Color,
}

impl Palette {
    /// Catppuccin Mocha — the default.
    pub fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),
            surface_dim: Color::Rgb(30, 30, 46),
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
        }
    }

    /// Tokyo Night — blue-purple aesthetic.
    pub fn tokyo_night() -> Self {
        Self {
            accent: Color::Rgb(122, 162, 247), // blue
            panel_bg: Color::Rgb(26, 27, 38),
            surface0: Color::Rgb(36, 40, 59),
            surface1: Color::Rgb(65, 72, 104),
            surface_dim: Color::Rgb(26, 27, 38),
            overlay0: Color::Rgb(86, 95, 137),
            overlay1: Color::Rgb(105, 113, 150),
            text: Color::Rgb(192, 202, 245),
            subtext0: Color::Rgb(169, 177, 214),
            mauve: Color::Rgb(187, 154, 247),
            green: Color::Rgb(158, 206, 106),
            yellow: Color::Rgb(224, 175, 104),
            red: Color::Rgb(247, 118, 142),
            blue: Color::Rgb(122, 162, 247),
            teal: Color::Rgb(125, 207, 255),
            peach: Color::Rgb(255, 158, 100),
        }
    }

    /// Dracula — purple/pink/green.
    pub fn dracula() -> Self {
        Self {
            accent: Color::Rgb(189, 147, 249), // purple
            panel_bg: Color::Rgb(40, 42, 54),
            surface0: Color::Rgb(68, 71, 90),
            surface1: Color::Rgb(98, 114, 164),
            surface_dim: Color::Rgb(40, 42, 54),
            overlay0: Color::Rgb(98, 114, 164),
            overlay1: Color::Rgb(130, 140, 180),
            text: Color::Rgb(248, 248, 242),
            subtext0: Color::Rgb(210, 210, 220),
            mauve: Color::Rgb(255, 121, 198), // pink
            green: Color::Rgb(80, 250, 123),
            yellow: Color::Rgb(241, 250, 140),
            red: Color::Rgb(255, 85, 85),
            blue: Color::Rgb(139, 233, 253), // cyan-ish
            teal: Color::Rgb(139, 233, 253),
            peach: Color::Rgb(255, 184, 108),
        }
    }

    /// Nord — frosty blue palette.
    pub fn nord() -> Self {
        Self {
            accent: Color::Rgb(136, 192, 208), // frost
            panel_bg: Color::Rgb(46, 52, 64),
            surface0: Color::Rgb(59, 66, 82),
            surface1: Color::Rgb(67, 76, 94),
            surface_dim: Color::Rgb(46, 52, 64),
            overlay0: Color::Rgb(76, 86, 106),
            overlay1: Color::Rgb(100, 110, 130),
            text: Color::Rgb(236, 239, 244),
            subtext0: Color::Rgb(216, 222, 233),
            mauve: Color::Rgb(180, 142, 173),
            green: Color::Rgb(163, 190, 140),
            yellow: Color::Rgb(235, 203, 139),
            red: Color::Rgb(191, 97, 106),
            blue: Color::Rgb(129, 161, 193),
            teal: Color::Rgb(143, 188, 187),
            peach: Color::Rgb(208, 135, 112),
        }
    }

    /// Gruvbox Dark — warm retro palette.
    pub fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(215, 153, 33), // yellow
            panel_bg: Color::Rgb(40, 40, 40),
            surface0: Color::Rgb(60, 56, 54),
            surface1: Color::Rgb(80, 73, 69),
            surface_dim: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(168, 153, 132),
            text: Color::Rgb(235, 219, 178),
            subtext0: Color::Rgb(213, 196, 161),
            mauve: Color::Rgb(211, 134, 155),
            green: Color::Rgb(184, 187, 38),
            yellow: Color::Rgb(250, 189, 47),
            red: Color::Rgb(251, 73, 52),
            blue: Color::Rgb(131, 165, 152),
            teal: Color::Rgb(142, 192, 124),
            peach: Color::Rgb(254, 128, 25),
        }
    }

    /// One Dark — Atom's classic dark theme.
    pub fn one_dark() -> Self {
        Self {
            accent: Color::Rgb(97, 175, 239), // blue
            panel_bg: Color::Rgb(40, 44, 52),
            surface0: Color::Rgb(44, 49, 58),
            surface1: Color::Rgb(62, 68, 81),
            surface_dim: Color::Rgb(40, 44, 52),
            overlay0: Color::Rgb(92, 99, 112),
            overlay1: Color::Rgb(115, 122, 135),
            text: Color::Rgb(171, 178, 191),
            subtext0: Color::Rgb(150, 156, 168),
            mauve: Color::Rgb(198, 120, 221),
            green: Color::Rgb(152, 195, 121),
            yellow: Color::Rgb(229, 192, 123),
            red: Color::Rgb(224, 108, 117),
            blue: Color::Rgb(97, 175, 239),
            teal: Color::Rgb(86, 182, 194),
            peach: Color::Rgb(209, 154, 102),
        }
    }

    /// Solarized Dark — Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210), // blue
            panel_bg: Color::Rgb(0, 43, 54),
            surface0: Color::Rgb(7, 54, 66),
            surface1: Color::Rgb(88, 110, 117),
            surface_dim: Color::Rgb(0, 43, 54),
            overlay0: Color::Rgb(88, 110, 117),
            overlay1: Color::Rgb(101, 123, 131),
            text: Color::Rgb(147, 161, 161),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
        }
    }

    /// Kanagawa — inspired by Katsushika Hokusai.
    pub fn kanagawa() -> Self {
        Self {
            accent: Color::Rgb(126, 156, 216), // blue
            panel_bg: Color::Rgb(31, 31, 40),
            surface0: Color::Rgb(42, 42, 55),
            surface1: Color::Rgb(54, 54, 70),
            surface_dim: Color::Rgb(31, 31, 40),
            overlay0: Color::Rgb(114, 113, 105),
            overlay1: Color::Rgb(135, 134, 125),
            text: Color::Rgb(220, 215, 186),
            subtext0: Color::Rgb(200, 195, 170),
            mauve: Color::Rgb(149, 127, 184),
            green: Color::Rgb(118, 148, 106),
            yellow: Color::Rgb(192, 163, 110),
            red: Color::Rgb(195, 64, 67),
            blue: Color::Rgb(126, 156, 216),
            teal: Color::Rgb(127, 180, 202),
            peach: Color::Rgb(255, 160, 102),
        }
    }

    /// Rosé Pine — muted, elegant.
    pub fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(196, 167, 231), // iris
            panel_bg: Color::Rgb(25, 23, 36),
            surface0: Color::Rgb(31, 29, 46),
            surface1: Color::Rgb(38, 35, 58),
            surface_dim: Color::Rgb(25, 23, 36),
            overlay0: Color::Rgb(110, 106, 134),
            overlay1: Color::Rgb(144, 140, 170),
            text: Color::Rgb(224, 222, 244),
            subtext0: Color::Rgb(200, 197, 220),
            mauve: Color::Rgb(196, 167, 231),  // iris
            green: Color::Rgb(49, 116, 143),   // pine
            yellow: Color::Rgb(246, 193, 119), // gold
            red: Color::Rgb(235, 111, 146),    // love
            blue: Color::Rgb(49, 116, 143),    // pine
            teal: Color::Rgb(156, 207, 216),   // foam
            peach: Color::Rgb(234, 154, 151),  // rose
        }
    }

    /// Resolve a theme by name. Returns None for unknown names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().replace([' ', '_'], "-").as_str() {
            "catppuccin" | "catppuccin-mocha" => Some(Self::catppuccin()),
            "tokyo-night" | "tokyonight" => Some(Self::tokyo_night()),
            "dracula" => Some(Self::dracula()),
            "nord" => Some(Self::nord()),
            "gruvbox" | "gruvbox-dark" => Some(Self::gruvbox()),
            "one-dark" | "onedark" => Some(Self::one_dark()),
            "solarized" | "solarized-dark" => Some(Self::solarized()),
            "kanagawa" => Some(Self::kanagawa()),
            "rose-pine" | "rosepine" => Some(Self::rose_pine()),
            _ => None,
        }
    }

    /// Apply custom color overrides on top of this palette.
    pub fn with_overrides(mut self, custom: &crate::config::CustomThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCardArea {
    pub ws_idx: usize,
    pub rect: Rect,
}

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
pub struct ViewState {
    pub sidebar_rect: Rect,
    pub workspace_card_areas: Vec<WorkspaceCardArea>,
    pub tab_bar_rect: Rect,
    pub tab_hit_areas: Vec<Rect>,
    pub tab_scroll_left_hit_area: Rect,
    pub tab_scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub terminal_area: Rect,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Onboarding,
    ReleaseNotes,
    Navigate,
    Terminal,
    RenameWorkspace,
    RenameTab,
    Resize,
    ConfirmClose,
    ContextMenu,
    Settings,
    GlobalMenu,
    KeybindHelp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentPanelScope {
    CurrentWorkspace,
    AllWorkspaces,
}

impl Default for AgentPanelScope {
    fn default() -> Self {
        Self::CurrentWorkspace
    }
}

// ---------------------------------------------------------------------------
// Settings UI state
// ---------------------------------------------------------------------------

/// Which section of the settings panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Theme,
    Sound,
    Toast,
}

impl SettingsSection {
    pub const ALL: &[Self] = &[Self::Theme, Self::Sound, Self::Toast];

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Sound => "sound",
            Self::Toast => "toasts",
        }
    }
}

/// All built-in theme names in display order.
pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "tokyo-night",
    "dracula",
    "nord",
    "gruvbox",
    "one-dark",
    "solarized",
    "kanagawa",
    "rose-pine",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListState {
    pub highlighted: usize,
}

impl MenuListState {
    pub fn new(highlighted: usize) -> Self {
        Self { highlighted }
    }

    pub fn move_prev(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.highlighted = (self.highlighted + 1).min(item_count - 1);
        }
    }

    pub fn hover(&mut self, idx: Option<usize>) {
        if let Some(idx) = idx {
            self.highlighted = idx;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionListState {
    pub selected: usize,
}

impl SelectionListState {
    pub fn new(selected: usize) -> Self {
        Self { selected }
    }

    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selected = (self.selected + 1).min(item_count - 1);
        }
    }

    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
    }
}

pub struct SettingsState {
    /// Which section tab is active.
    pub section: SettingsSection,
    /// Selected item index within the current section.
    pub list: SelectionListState,
    /// The palette before opening settings (for cancel/restore).
    pub original_palette: Option<Palette>,
    /// The theme name before opening settings.
    pub original_theme: Option<String>,
}

pub(crate) enum DragTarget {
    WorkspaceReorder {
        source_ws_idx: usize,
        insert_idx: Option<usize>,
    },
    TabReorder {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: Option<usize>,
    },
    WorkspaceListScrollbar {
        grab_row_offset: u16,
    },
    AgentPanelScrollbar {
        grab_row_offset: u16,
    },
    PaneSplit {
        path: Vec<bool>,
        direction: Direction,
        area: Rect,
    },
    PaneScrollbar {
        pane_id: crate::layout::PaneId,
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    KeybindHelpScrollbar {
        grab_row_offset: u16,
    },
    SidebarDivider,
    SidebarSectionDivider,
}

/// Active mouse drag on a split border or sidebar divider.
pub(crate) struct DragState {
    pub target: DragTarget,
}

pub(crate) struct WorkspacePressState {
    pub ws_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct TabPressState {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuKind {
    Workspace { ws_idx: usize },
    Tab { ws_idx: usize, tab_idx: usize },
    Pane,
}

/// Right-click context menu state.
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub x: u16,
    pub y: u16,
    pub list: MenuListState,
}

impl ContextMenuState {
    pub fn items(&self) -> &'static [&'static str] {
        match self.kind {
            ContextMenuKind::Workspace { .. } => &["Rename", "Close"],
            ContextMenuKind::Tab { .. } => &["New tab", "Rename", "Close"],
            ContextMenuKind::Pane => &[
                "Split vertical",
                "Split horizontal",
                "Fullscreen",
                "Close pane",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    pub kind: ToastKind,
    pub title: String,
    pub context: String,
}

pub struct ReleaseNotesState {
    pub version: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct KeybindHelpState {
    pub scroll: u16,
}

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
pub struct AppState {
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    /// In persistence mode, client quit actions detach instead of stopping the server.
    pub quit_detaches: bool,
    /// Set when the current client should detach from the persistent session.
    /// The server's event loop checks this and handles client detach.
    pub detach_requested: bool,
    pub request_new_workspace: bool,
    pub request_new_tab: bool,
    pub request_reload_keybinds: bool,
    pub creating_new_tab: bool,
    pub requested_new_tab_name: Option<String>,
    pub request_complete_onboarding: bool,
    pub name_input: String,
    pub name_input_replace_on_type: bool,
    pub onboarding_step: usize,
    pub onboarding_list: SelectionListState,
    pub release_notes: Option<ReleaseNotesState>,
    pub keybind_help: KeybindHelpState,
    pub workspace_scroll: usize,
    pub agent_panel_scroll: usize,
    pub tab_scroll: usize,
    pub tab_scroll_follow_active: bool,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    pub(crate) workspace_press: Option<WorkspacePressState>,
    pub(crate) tab_press: Option<TabPressState>,
    pub selection: Option<Selection>,
    pub context_menu: Option<ContextMenuState>,
    // Notifications
    pub update_available: Option<String>,
    pub latest_release_notes_available: bool,
    pub update_dismissed: bool,
    pub config_diagnostic: Option<String>,
    pub toast: Option<ToastNotification>,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    pub default_sidebar_width: u16,
    pub sidebar_width: u16,
    pub sidebar_width_auto: bool,
    pub sidebar_collapsed: bool,
    /// Ratio of sidebar height allocated to the workspaces section.
    pub sidebar_section_split: f32,
    pub agent_panel_scope: AgentPanelScope,
    pub confirm_close: bool,
    pub pane_scrollback_limit_bytes: usize,
    #[allow(dead_code)] // kept for backward compat; palette.accent is the source of truth
    pub accent: Color,
    pub sound: SoundConfig,
    pub toast_config: ToastConfig,
    pub keybinds: Keybinds,
    /// Frame counter for spinner animations (wraps around).
    pub spinner_tick: u32,
    /// UI color palette — all sidebar/UI colors centralized for theming.
    pub palette: Palette,
    /// Currently applied theme name (for settings UI).
    pub theme_name: String,
    /// Settings panel state.
    pub settings: SettingsState,
    /// Highlight state for the bottom-right global launcher menu.
    pub global_menu: MenuListState,
    /// Resolved host terminal default colors for theming embedded panes.
    pub host_terminal_theme: TerminalTheme,
    /// Set when a persisted session snapshot would change.
    pub session_dirty: bool,
}

impl AppState {
    pub(crate) fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound.enabled
    }

    pub fn is_prefix(&self, key: &crossterm::event::KeyEvent) -> bool {
        key_matches(key, self.prefix_code, self.prefix_mods)
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (24, 80)
        }
    }
}

pub fn key_matches(
    key: &crossterm::event::KeyEvent,
    expected_code: KeyCode,
    expected_mods: KeyModifiers,
) -> bool {
    if key.modifiers != expected_mods {
        return false;
    }

    match (key.code, expected_code) {
        (KeyCode::Char(actual), KeyCode::Char(expected))
            if actual.is_ascii_alphabetic() && expected.is_ascii_alphabetic() =>
        {
            actual.eq_ignore_ascii_case(&expected)
        }
        (actual, expected) => actual == expected,
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
            quit_detaches: false,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_reload_keybinds: false,
            creating_new_tab: false,
            requested_new_tab_name: None,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            onboarding_step: 0,
            onboarding_list: SelectionListState::new(1),
            release_notes: None,
            keybind_help: KeybindHelpState { scroll: 0 },
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            view: ViewState {
                sidebar_rect: Rect::default(),
                workspace_card_areas: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            workspace_press: None,
            tab_press: None,
            selection: None,
            context_menu: None,
            update_available: None,
            latest_release_notes_available: false,
            update_dismissed: false,
            config_diagnostic: None,
            toast: None,
            prefix_code: KeyCode::Char('b'),
            prefix_mods: KeyModifiers::CONTROL,
            default_sidebar_width: 26,
            sidebar_width: 26,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            sidebar_section_split: 0.5,
            agent_panel_scope: AgentPanelScope::CurrentWorkspace,
            confirm_close: true,
            pane_scrollback_limit_bytes: crate::config::DEFAULT_SCROLLBACK_LIMIT_BYTES,
            accent: Color::Cyan,
            sound: SoundConfig {
                enabled: false,
                ..SoundConfig::default()
            },
            toast_config: ToastConfig::default(),
            keybinds: Keybinds {
                new_workspace: (KeyCode::Char('n'), KeyModifiers::empty()),
                new_workspace_label: "n".into(),
                rename_workspace: (KeyCode::Char('n'), KeyModifiers::SHIFT),
                rename_workspace_label: "shift+n".into(),
                close_workspace: (KeyCode::Char('d'), KeyModifiers::SHIFT),
                close_workspace_label: "shift+d".into(),
                detach: None,
                detach_label: None,
                previous_workspace: None,
                previous_workspace_label: None,
                next_workspace: None,
                next_workspace_label: None,
                new_tab: (KeyCode::Char('c'), KeyModifiers::empty()),
                new_tab_label: "c".into(),
                rename_tab: None,
                rename_tab_label: None,
                previous_tab: None,
                previous_tab_label: None,
                next_tab: None,
                next_tab_label: None,
                close_tab: None,
                close_tab_label: None,
                focus_pane_left: None,
                focus_pane_left_label: None,
                focus_pane_down: None,
                focus_pane_down_label: None,
                focus_pane_up: None,
                focus_pane_up_label: None,
                focus_pane_right: None,
                focus_pane_right_label: None,
                split_vertical: (KeyCode::Char('v'), KeyModifiers::empty()),
                split_vertical_label: "v".into(),
                split_horizontal: (KeyCode::Char('-'), KeyModifiers::empty()),
                split_horizontal_label: "-".into(),
                close_pane: (KeyCode::Char('x'), KeyModifiers::empty()),
                close_pane_label: "x".into(),
                fullscreen: (KeyCode::Char('f'), KeyModifiers::empty()),
                fullscreen_label: "f".into(),
                resize_mode: (KeyCode::Char('r'), KeyModifiers::empty()),
                resize_mode_label: "r".into(),
                toggle_sidebar: (KeyCode::Char('b'), KeyModifiers::empty()),
                toggle_sidebar_label: "b".into(),
                custom_commands: Vec::new(),
            },
            spinner_tick: 0,
            palette: Palette::catppuccin(),
            theme_name: "catppuccin".to_string(),
            settings: SettingsState {
                section: SettingsSection::Theme,
                list: SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
            },
            global_menu: MenuListState::new(0),
            host_terminal_theme: TerminalTheme::default(),
            session_dirty: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn key_matches_requires_exact_modifiers() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        assert!(!key_matches(
            &KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
    }

    #[test]
    fn key_matches_letters_case_insensitively() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            KeyCode::Char('b'),
            KeyModifiers::SHIFT,
        ));
    }
}
