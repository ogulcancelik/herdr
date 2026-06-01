use std::num::NonZeroUsize;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    BindingConfig, CommandKeybindConfig, SoundConfig, ThemeConfig, DEFAULT_MOBILE_WIDTH_THRESHOLD,
    DEFAULT_MOUSE_SCROLL_LINES, DEFAULT_SCROLLBACK_LIMIT_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToastDelivery {
    #[default]
    Off,
    Herdr,
    Terminal,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentPanelScopeConfig {
    Current,
    #[default]
    All,
}

impl AgentPanelScopeConfig {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToastConfig {
    pub delivery: ToastDelivery,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NewTerminalCwdConfig {
    #[default]
    Follow,
    Home,
    Current,
    Path(String),
}

impl<'de> Deserialize<'de> for NewTerminalCwdConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim() {
            "" | "follow" => Ok(Self::Follow),
            "home" => Ok(Self::Home),
            "current" => Ok(Self::Current),
            _ => Ok(Self::Path(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellModeConfig {
    #[default]
    Auto,
    Login,
    NonLogin,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// Executable used for new interactive panes. Empty means SHELL, then /bin/sh.
    pub default_shell: String,
    /// Startup mode for new interactive pane shells.
    pub shell_mode: ShellModeConfig,
    /// CWD policy for new interactive panes, tabs, and workspaces.
    pub new_cwd: NewTerminalCwdConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Resume supported AI-agent panes into their native conversation sessions
    /// when restoring a Herdr session. Default: false.
    pub resume_agents_on_restore: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReloadStatus {
    Applied,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ConfigReloadReport {
    pub status: ConfigReloadStatus,
    pub diagnostics: Vec<String>,
}

/// Validate `[ui]` sidebar bound configuration.
///
/// Returns `Some((min, max))` when `min <= max`, `None` otherwise. The two
/// values are funneled through this helper before they reach any
/// `u16::clamp(min, max)` call site (`u16::clamp` panics when `min > max`).
pub fn validated_sidebar_bounds(min: u16, max: u16) -> Option<(u16, u16)> {
    if min <= max {
        Some((min, max))
    } else {
        None
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub theme: ThemeConfig,
    pub terminal: TerminalConfig,
    pub session: SessionConfig,
    pub keys: KeysConfig,
    pub ui: UiConfig,
    pub worktrees: WorktreesConfig,
    pub advanced: AdvancedConfig,
    pub experimental: ExperimentalConfig,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub diagnostics: Vec<String>,
    pub invalid_sections: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Prefix key to enter prefix mode (e.g. "ctrl+b", "f12", "esc").
    pub prefix: String,
    /// Open keybinding help. Default: "prefix+?"
    pub help: BindingConfig,
    /// Open settings. Default: "prefix+s"
    pub settings: BindingConfig,
    /// Create a new workspace. Default: "prefix+shift+n"
    pub new_workspace: BindingConfig,
    /// Create a Git worktree from the selected workspace. Default: "prefix+shift+g"
    pub new_worktree: BindingConfig,
    /// Open an existing Git worktree from the selected workspace. Unset by default.
    pub open_worktree: BindingConfig,
    /// Delete the selected managed worktree checkout after confirmation. Unset by default.
    pub remove_worktree: BindingConfig,
    /// Rename the selected workspace. Default: "prefix+shift+w"
    pub rename_workspace: BindingConfig,
    /// Close the selected workspace. Default: "prefix+shift+d"
    pub close_workspace: BindingConfig,
    /// Open the workspace navigation surface. Default: "prefix+w"
    pub workspace_picker: BindingConfig,
    /// Open the session navigator. Default: "prefix+g"
    pub goto: BindingConfig,
    /// Move workspace selection up in navigate mode. Default: "up".
    pub navigate_workspace_up: BindingConfig,
    /// Move workspace selection down in navigate mode. Default: "down".
    pub navigate_workspace_down: BindingConfig,
    /// Focus the pane to the left in navigate mode. Default: "h". Left arrow is always an alias.
    pub navigate_pane_left: BindingConfig,
    /// Focus the pane below in navigate mode. Default: "j".
    pub navigate_pane_down: BindingConfig,
    /// Focus the pane above in navigate mode. Default: "k".
    pub navigate_pane_up: BindingConfig,
    /// Focus the pane to the right in navigate mode. Default: "l". Right arrow is always an alias.
    pub navigate_pane_right: BindingConfig,
    /// Detach from server/client mode, or exit --no-session mode. Default: "prefix+q".
    pub detach: BindingConfig,
    /// Reload config.toml in the running app/server. Default: "prefix+shift+r".
    pub reload_config: BindingConfig,
    /// Focus the currently visible notification target. Default: "prefix+o".
    pub open_notification_target: BindingConfig,
    /// Select the previous workspace. Unset by default.
    pub previous_workspace: BindingConfig,
    /// Select the next workspace. Unset by default.
    pub next_workspace: BindingConfig,
    /// Focus the previous agent shown in the agent panel. Unset by default.
    pub previous_agent: BindingConfig,
    /// Focus the next agent shown in the agent panel. Unset by default.
    pub next_agent: BindingConfig,
    /// Focus an agent by index 1-9. Unset by default.
    pub focus_agent: BindingConfig,
    /// Create a new tab in the active workspace. Default: "prefix+c"
    pub new_tab: BindingConfig,
    /// Rename the active tab. Default: "prefix+shift+t".
    pub rename_tab: BindingConfig,
    /// Select the previous tab. Default: "prefix+p".
    pub previous_tab: BindingConfig,
    /// Select the next tab. Default: "prefix+n".
    pub next_tab: BindingConfig,
    /// Switch to tab 1-9. Default: "prefix+1..9".
    pub switch_tab: BindingConfig,
    /// Switch to workspace 1-9 from prefix mode. Unset by default.
    pub switch_workspace: BindingConfig,
    /// Close the active tab. Default: "prefix+shift+x".
    pub close_tab: BindingConfig,
    /// Rename the focused pane. Default: "prefix+shift+p".
    pub rename_pane: BindingConfig,
    /// Open the focused pane scrollback in $EDITOR. Default: "prefix+e".
    pub edit_scrollback: BindingConfig,
    /// Enter keyboard copy mode for the focused pane. Default: "prefix+[".
    pub copy_mode: BindingConfig,
    /// Focus the pane to the left. Default: "prefix+h".
    pub focus_pane_left: BindingConfig,
    /// Focus the pane below. Default: "prefix+j".
    pub focus_pane_down: BindingConfig,
    /// Focus the pane above. Default: "prefix+k".
    pub focus_pane_up: BindingConfig,
    /// Focus the pane to the right. Default: "prefix+l".
    pub focus_pane_right: BindingConfig,
    /// Cycle to the next pane. Default: "prefix+tab".
    pub cycle_pane_next: BindingConfig,
    /// Cycle to the previous pane. Default: "prefix+shift+tab".
    pub cycle_pane_previous: BindingConfig,
    /// Focus the last focused pane across workspaces and tabs. Unset by default.
    pub last_pane: BindingConfig,
    /// Split pane vertically (side by side). Default: "prefix+v"
    pub split_vertical: BindingConfig,
    /// Split pane horizontally (stacked). Default: "prefix+minus"
    pub split_horizontal: BindingConfig,
    /// Close the focused pane. Default: "prefix+x"
    pub close_pane: BindingConfig,
    /// Toggle zoom for the focused pane. Default: "prefix+z"
    #[serde(alias = "fullscreen")]
    pub zoom: BindingConfig,
    /// Enter resize mode. Default: "prefix+r"
    pub resize_mode: BindingConfig,
    /// Toggle sidebar collapse. Default: "prefix+b"
    pub toggle_sidebar: BindingConfig,
    /// Optional indexed shortcuts expanded over number keys 1-9.
    pub indexed: IndexedKeysConfig,
    /// Prefix-mode custom command bindings.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<CommandKeybindConfig>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IndexedKeysConfig {
    /// Modifier combo for tab shortcuts 1-9. Unset by default.
    pub tabs: String,
    /// Modifier combo for workspace shortcuts 1-9. Unset by default.
    pub workspaces: String,
    /// Modifier combo for agent shortcuts 1-9. Unset by default.
    pub agents: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct WorktreesConfig {
    /// Root directory under which Herdr creates <repo>/<branch-slug> checkouts.
    pub directory: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub sidebar_width: u16,
    /// Minimum sidebar width (columns) when expanded. Default: 18.
    pub sidebar_min_width: u16,
    /// Maximum sidebar width (columns) when expanded. Default: 36.
    pub sidebar_max_width: u16,
    /// Terminal width at or below which Herdr uses the mobile single-column layout. Default: 64.
    pub mobile_width_threshold: u16,
    /// Capture mouse input for Herdr's mouse UI. Default: true.
    pub mouse_capture: bool,
    /// Force a full host-terminal redraw when the outer terminal regains focus. Default: true.
    pub redraw_on_focus_gained: bool,
    /// Lines to scroll per mouse wheel notch. Default: 3.
    pub mouse_scroll_lines: Option<NonZeroUsize>,
    /// Ask for confirmation before closing a workspace. Default: true.
    pub confirm_close: bool,
    /// Ask for a tab name before creating a new tab. Default: true.
    pub prompt_new_tab_name: bool,
    /// Show agent labels in split pane borders when no manual pane label is set. Default: false.
    pub show_agent_labels_on_pane_borders: bool,
    /// Agent sidebar scope. Saved values are "current" or "all". Default: "all".
    pub agent_panel_scope: AgentPanelScopeConfig,
    /// Accent color for highlights, borders, and navigation UI.
    /// Accepts hex (#89b4fa), named colors (cyan, blue), or RGB (rgb(137,180,250)).
    pub accent: String,
    /// Optional visual toast notifications for background workspace events.
    pub toast: ToastConfig,
    /// Play sounds when agents change state in background workspaces.
    pub sound: SoundConfig,
}

/// Cursor shape (DECSCUSR) used for the forced IME anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImeCursorShape {
    Block,
    #[default]
    SteadyBlock,
    Underline,
    SteadyUnderline,
    Bar,
    SteadyBar,
}

impl ImeCursorShape {
    /// Convert to DECSCUSR parameter (1–6).
    pub fn to_decscusr(self) -> u8 {
        match self {
            Self::Block => 1,
            Self::SteadyBlock => 2,
            Self::Underline => 3,
            Self::SteadyUnderline => 4,
            Self::Bar => 5,
            Self::SteadyBar => 6,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AdvancedConfig {
    /// Maximum scrollback buffer size in bytes retained per pane terminal. Default: 10000000.
    #[serde(alias = "scrollback_lines")]
    pub scrollback_limit_bytes: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ExperimentalConfig {
    /// Allow launching herdr inside an existing herdr pane. Default: false.
    pub allow_nested: bool,
    /// Experimental local Kitty graphics rendering for attached clients. Default: false.
    pub kitty_graphics: bool,
    /// Persist pane screen history to session-history.json. Default: false.
    pub pane_history: bool,
    /// Expose the focused pane's cursor anchor to the outer terminal even when
    /// the pane requested `?25l`, so macOS native input methods keep tracking
    /// the candidate window when TUIs paint their own cursor (Claude Code, pi,
    /// codex, etc.). Default: false.
    ///
    /// When the pane reports no cursor position, falls back to the pane's
    /// top-left so a stable IME anchor is always available.
    ///
    /// Trade-off when enabled: an extra hardware cursor will be visible in the
    /// outer terminal for apps that hide the cursor without painting a
    /// replacement (vim normal mode, etc.). See #149.
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    /// Restrict `reveal_hidden_cursor_for_cjk_ime` to focused panes whose
    /// detected agent matches one of these names (case-insensitive). Empty
    /// list means apply to any focused pane. Unknown agent names are ignored;
    /// if the list contains no valid names, the reveal does not apply.
    /// Accepted names: pi, claude, codex, gemini, cursor, cline, opencode,
    /// copilot, kimi, kiro, droid, amp, grok, hermes. Default: empty.
    pub cjk_ime_agents: Vec<String>,
    /// Cursor shape rendered for the IME anchor when
    /// `reveal_hidden_cursor_for_cjk_ime` is enabled. Default: "steady_block".
    pub cjk_ime_cursor_shape: ImeCursorShape,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl+b".into(),
            help: BindingConfig::one("prefix+?"),
            settings: BindingConfig::one("prefix+s"),
            new_workspace: BindingConfig::one("prefix+shift+n"),
            new_worktree: BindingConfig::one("prefix+shift+g"),
            open_worktree: BindingConfig::empty(),
            remove_worktree: BindingConfig::empty(),
            rename_workspace: BindingConfig::one("prefix+shift+w"),
            close_workspace: BindingConfig::one("prefix+shift+d"),
            workspace_picker: BindingConfig::one("prefix+w"),
            goto: BindingConfig::one("prefix+g"),
            navigate_workspace_up: BindingConfig::one("up"),
            navigate_workspace_down: BindingConfig::one("down"),
            navigate_pane_left: BindingConfig::one("h"),
            navigate_pane_down: BindingConfig::one("j"),
            navigate_pane_up: BindingConfig::one("k"),
            navigate_pane_right: BindingConfig::one("l"),
            detach: BindingConfig::one("prefix+q"),
            reload_config: BindingConfig::one("prefix+shift+r"),
            open_notification_target: BindingConfig::one("prefix+o"),
            previous_workspace: BindingConfig::empty(),
            next_workspace: BindingConfig::empty(),
            previous_agent: BindingConfig::empty(),
            next_agent: BindingConfig::empty(),
            focus_agent: BindingConfig::empty(),
            new_tab: BindingConfig::one("prefix+c"),
            rename_tab: BindingConfig::one("prefix+shift+t"),
            previous_tab: BindingConfig::one("prefix+p"),
            next_tab: BindingConfig::one("prefix+n"),
            switch_tab: BindingConfig::one("prefix+1..9"),
            switch_workspace: BindingConfig::empty(),
            close_tab: BindingConfig::one("prefix+shift+x"),
            rename_pane: BindingConfig::one("prefix+shift+p"),
            edit_scrollback: BindingConfig::one("prefix+e"),
            copy_mode: BindingConfig::one("prefix+["),
            focus_pane_left: BindingConfig::one("prefix+h"),
            focus_pane_down: BindingConfig::one("prefix+j"),
            focus_pane_up: BindingConfig::one("prefix+k"),
            focus_pane_right: BindingConfig::one("prefix+l"),
            cycle_pane_next: BindingConfig::one("prefix+tab"),
            cycle_pane_previous: BindingConfig::one("prefix+shift+tab"),
            last_pane: BindingConfig::empty(),
            split_vertical: BindingConfig::one("prefix+v"),
            split_horizontal: BindingConfig::one("prefix+minus"),
            close_pane: BindingConfig::one("prefix+x"),
            zoom: BindingConfig::one("prefix+z"),
            resize_mode: BindingConfig::one("prefix+r"),
            toggle_sidebar: BindingConfig::one("prefix+b"),
            indexed: IndexedKeysConfig::default(),
            command: Vec::new(),
        }
    }
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        Self {
            directory: "~/.herdr/worktrees".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 36,
            mobile_width_threshold: DEFAULT_MOBILE_WIDTH_THRESHOLD,
            mouse_capture: true,
            redraw_on_focus_gained: true,
            mouse_scroll_lines: None,
            confirm_close: true,
            prompt_new_tab_name: true,
            show_agent_labels_on_pane_borders: false,
            agent_panel_scope: AgentPanelScopeConfig::All,
            accent: "cyan".into(),
            toast: ToastConfig::default(),
            sound: SoundConfig::default(),
        }
    }
}

impl UiConfig {
    pub fn mouse_scroll_lines(&self) -> usize {
        self.mouse_scroll_lines
            .map(NonZeroUsize::get)
            .unwrap_or(DEFAULT_MOUSE_SCROLL_LINES)
    }
}

impl Default for ToastConfig {
    fn default() -> Self {
        Self {
            delivery: ToastDelivery::Off,
        }
    }
}

impl<'de> Deserialize<'de> for ToastConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct RawToastConfig {
            delivery: Option<ToastDelivery>,
            enabled: Option<bool>,
        }

        let raw = RawToastConfig::deserialize(deserializer)?;
        let legacy_delivery = match raw.enabled {
            Some(true) => ToastDelivery::Herdr,
            Some(false) | None => ToastDelivery::Off,
        };
        let delivery = raw.delivery.unwrap_or(legacy_delivery);
        Ok(Self { delivery })
    }
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            scrollback_limit_bytes: DEFAULT_SCROLLBACK_LIMIT_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_default_shell_defaults_empty_and_parses() {
        let default_config = Config::default();
        assert!(default_config.terminal.default_shell.is_empty());
        assert_eq!(default_config.terminal.shell_mode, ShellModeConfig::Auto);

        let toml = r#"
[terminal]
default_shell = "nu"
shell_mode = "non_login"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.terminal.default_shell, "nu");
        assert_eq!(config.terminal.shell_mode, ShellModeConfig::NonLogin);
    }

    #[test]
    fn terminal_new_cwd_defaults_follow_and_parses() {
        let default_config = Config::default();
        assert_eq!(
            default_config.terminal.new_cwd,
            NewTerminalCwdConfig::Follow
        );

        let config: Config = toml::from_str(
            r#"
[terminal]
new_cwd = "home"
"#,
        )
        .unwrap();
        assert_eq!(config.terminal.new_cwd, NewTerminalCwdConfig::Home);

        let config: Config = toml::from_str(
            r#"
[terminal]
new_cwd = "~/Projects"
"#,
        )
        .unwrap();
        assert_eq!(
            config.terminal.new_cwd,
            NewTerminalCwdConfig::Path("~/Projects".into())
        );
    }

    #[test]
    fn resume_agents_on_restore_defaults_off_and_parses() {
        let default_config = Config::default();
        assert!(!default_config.session.resume_agents_on_restore);

        let toml = r#"
[session]
resume_agents_on_restore = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.session.resume_agents_on_restore);
    }

    #[test]
    fn agent_panel_scope_config_parses() {
        let toml = r#"
[ui]
agent_panel_scope = "all"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.agent_panel_scope, AgentPanelScopeConfig::All);
    }

    #[test]
    fn pane_border_agent_labels_default_off_and_parse() {
        let default_config = Config::default();
        assert!(!default_config.ui.show_agent_labels_on_pane_borders);

        let toml = r#"
[ui]
show_agent_labels_on_pane_borders = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.show_agent_labels_on_pane_borders);
    }

    #[test]
    fn worktrees_directory_defaults_and_parses() {
        let default_config = Config::default();
        assert_eq!(default_config.worktrees.directory, "~/.herdr/worktrees");

        let toml = r#"
[worktrees]
directory = "~/Projects/herdr-worktrees"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.worktrees.directory, "~/Projects/herdr-worktrees");
    }

    #[test]
    fn prompt_new_tab_name_defaults_on_and_parses() {
        let default_config = Config::default();
        assert!(default_config.ui.prompt_new_tab_name);

        let toml = r#"
[ui]
prompt_new_tab_name = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.prompt_new_tab_name);
    }

    #[test]
    fn reveal_hidden_cursor_for_cjk_ime_default_off_and_parse() {
        let default_config = Config::default();
        assert!(!default_config.experimental.reveal_hidden_cursor_for_cjk_ime);

        let toml = r#"
[experimental]
reveal_hidden_cursor_for_cjk_ime = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.reveal_hidden_cursor_for_cjk_ime);
    }

    #[test]
    fn cjk_ime_cursor_shape_default_steady_block_and_parse() {
        let default_config = Config::default();
        assert_eq!(
            default_config.experimental.cjk_ime_cursor_shape,
            ImeCursorShape::SteadyBlock
        );

        let toml = r#"
[experimental]
cjk_ime_cursor_shape = "bar"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.experimental.cjk_ime_cursor_shape,
            ImeCursorShape::Bar
        );
    }

    #[test]
    fn cjk_ime_agents_default_empty_and_parse() {
        let default_config = Config::default();
        assert!(default_config.experimental.cjk_ime_agents.is_empty());

        let toml = r#"
[experimental]
cjk_ime_agents = ["claude", "codex"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            config.experimental.cjk_ime_agents,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn sidebar_bounds_default_and_parse() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.sidebar_min_width, 18);
        assert_eq!(default_config.ui.sidebar_max_width, 36);
        assert_eq!(
            default_config.ui.mobile_width_threshold,
            DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        let toml = r#"
[ui]
sidebar_min_width = 12
sidebar_max_width = 80
mobile_width_threshold = 96
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.sidebar_min_width, 12);
        assert_eq!(config.ui.sidebar_max_width, 80);
        assert_eq!(config.ui.mobile_width_threshold, 96);
    }

    #[test]
    fn validated_sidebar_bounds_rejects_inverted() {
        assert_eq!(validated_sidebar_bounds(18, 36), Some((18, 36)));
        assert_eq!(validated_sidebar_bounds(20, 20), Some((20, 20)));
        assert_eq!(validated_sidebar_bounds(0, u16::MAX), Some((0, u16::MAX)));
        assert_eq!(validated_sidebar_bounds(50, 30), None);
        assert_eq!(validated_sidebar_bounds(u16::MAX, 0), None);
    }

    #[test]
    fn mouse_capture_default_on_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.mouse_capture);

        let toml = r#"
[ui]
mouse_capture = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.mouse_capture);
    }

    #[test]
    fn redraw_on_focus_gained_default_on_and_parse() {
        let default_config = Config::default();
        assert!(default_config.ui.redraw_on_focus_gained);

        let toml = r#"
[ui]
redraw_on_focus_gained = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(!config.ui.redraw_on_focus_gained);
    }

    #[test]
    fn mouse_scroll_lines_defaults_to_three_and_parses() {
        let default_config = Config::default();
        assert_eq!(
            default_config.ui.mouse_scroll_lines(),
            DEFAULT_MOUSE_SCROLL_LINES
        );

        let toml = r#"
[ui]
mouse_scroll_lines = 1
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.mouse_scroll_lines(), 1);
    }

    #[test]
    fn mouse_scroll_lines_rejects_zero() {
        let toml = r#"
[ui]
mouse_scroll_lines = 0
"#;
        assert!(toml::from_str::<Config>(toml).is_err());
    }

    #[test]
    fn toast_config_parses() {
        let toml = r#"
[ui.toast]
delivery = "terminal"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Terminal);
    }

    #[test]
    fn toast_config_parses_system_delivery() {
        let toml = r#"
[ui.toast]
delivery = "system"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::System);
    }

    #[test]
    fn toast_config_legacy_enabled_true_maps_to_herdr() {
        let toml = r#"
[ui.toast]
enabled = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Herdr);
    }

    #[test]
    fn toast_config_legacy_enabled_false_maps_to_off() {
        let toml = r#"
[ui.toast]
enabled = false
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Off);
    }

    #[test]
    fn toast_config_delivery_wins_over_legacy_enabled() {
        let toml = r#"
[ui.toast]
enabled = true
delivery = "terminal"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.ui.toast.delivery, ToastDelivery::Terminal);
    }

    #[test]
    fn missing_onboarding_shows_setup() {
        let config = Config::default();
        assert!(config.should_show_onboarding());
    }

    #[test]
    fn onboarding_false_skips_setup() {
        let config: Config = toml::from_str("onboarding = false").unwrap();
        assert!(!config.should_show_onboarding());
    }

    #[test]
    fn advanced_defaults_include_scrollback_limit_bytes() {
        let config = Config::default();
        assert_eq!(
            config.advanced.scrollback_limit_bytes,
            DEFAULT_SCROLLBACK_LIMIT_BYTES
        );
    }

    #[test]
    fn pane_history_persistence_is_opt_in() {
        assert!(!Config::default().experimental.pane_history);

        let toml = r#"
[experimental]
pane_history = true
"#;
        let config: Config = toml::from_str(toml).unwrap();

        assert!(config.experimental.pane_history);
    }

    #[test]
    fn kitty_graphics_default_off_and_parse() {
        let config = Config::default();
        assert!(!config.experimental.kitty_graphics);

        let toml = r#"
[experimental]
kitty_graphics = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.kitty_graphics);
    }

    #[test]
    fn experimental_config_parses() {
        let toml = r#"
[experimental]
allow_nested = true
kitty_graphics = true
pane_history = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.experimental.allow_nested);
        assert!(config.experimental.kitty_graphics);
        assert!(config.experimental.pane_history);
    }

    #[test]
    fn advanced_config_parses() {
        let toml = r#"
[advanced]
scrollback_limit_bytes = 12345
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.advanced.scrollback_limit_bytes, 12345);
    }

    #[test]
    fn advanced_legacy_scrollback_lines_alias_parses() {
        let toml = r#"
[advanced]
scrollback_lines = 12345
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.advanced.scrollback_limit_bytes, 12345);
    }
}
