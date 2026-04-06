use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

pub const CONFIG_PATH_ENV_VAR: &str = "HERDR_CONFIG_PATH";
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;
use tracing::warn;

pub fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join(app_dir_name())
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(format!(".config/{}", app_dir_name()))
    } else {
        PathBuf::from(format!("/tmp/{}", app_dir_name()))
    }
}

use crate::detect::Agent;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ToastConfig {
    pub enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub theme: ThemeConfig,
    pub keys: KeysConfig,
    pub ui: UiConfig,
    pub advanced: AdvancedConfig,
}

/// Theme configuration: pick a built-in or override individual tokens.
///
/// ```toml
/// [theme]
/// name = "tokyo-night"  # built-in: catppuccin, tokyo-night, dracula, nord, etc.
///
/// [theme.custom]        # override individual tokens on top of the base
/// accent = "#f5c2e7"
/// red = "#ff6188"
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Built-in theme name. Default: "catppuccin".
    pub name: Option<String>,
    /// Custom overrides — applied on top of the selected base theme.
    pub custom: Option<CustomThemeColors>,
}

/// Per-token color overrides. All fields optional — only set what you want to change.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CustomThemeColors {
    pub accent: Option<String>,
    pub panel_bg: Option<String>,
    pub surface0: Option<String>,
    pub surface1: Option<String>,
    pub surface_dim: Option<String>,
    pub overlay0: Option<String>,
    pub overlay1: Option<String>,
    pub text: Option<String>,
    pub subtext0: Option<String>,
    pub mauve: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub red: Option<String>,
    pub blue: Option<String>,
    pub teal: Option<String>,
    pub peach: Option<String>,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Prefix key to toggle navigate mode (e.g. "ctrl+b", "f12", "esc").
    pub prefix: String,
    /// Create a new workspace. Default: "n"
    pub new_workspace: String,
    /// Rename the selected workspace. Default: "shift+n"
    pub rename_workspace: String,
    /// Close the selected workspace. Default: "d"
    pub close_workspace: String,
    /// Select the previous workspace. Unset by default.
    pub previous_workspace: String,
    /// Select the next workspace. Unset by default.
    pub next_workspace: String,
    /// Create a new tab in the active workspace. Default: "c"
    pub new_tab: String,
    /// Rename the active tab. Unset by default.
    pub rename_tab: String,
    /// Select the previous tab. Unset by default.
    pub previous_tab: String,
    /// Select the next tab. Unset by default.
    pub next_tab: String,
    /// Close the active tab. Unset by default.
    pub close_tab: String,
    /// Focus the pane to the left in terminal mode. Unset by default.
    pub focus_pane_left: String,
    /// Focus the pane below in terminal mode. Unset by default.
    pub focus_pane_down: String,
    /// Focus the pane above in terminal mode. Unset by default.
    pub focus_pane_up: String,
    /// Focus the pane to the right in terminal mode. Unset by default.
    pub focus_pane_right: String,
    /// Split pane vertically (side by side). Default: "v"
    pub split_vertical: String,
    /// Split pane horizontally (stacked). Default: "-"
    pub split_horizontal: String,
    /// Close the focused pane. Default: "x"
    pub close_pane: String,
    /// Toggle fullscreen for the focused pane. Default: "f"
    pub fullscreen: String,
    /// Enter resize mode. Default: "r"
    pub resize_mode: String,
    /// Toggle sidebar collapse. Default: "b"
    pub toggle_sidebar: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub sidebar_width: u16,
    /// Ask for confirmation before closing a workspace. Default: true.
    pub confirm_close: bool,
    /// Accent color for highlights, borders, and navigation UI.
    /// Accepts hex (#89b4fa), named colors (cyan, blue), or RGB (rgb(137,180,250)).
    pub accent: String,
    /// Optional visual toast notifications for background workspace events.
    pub toast: ToastConfig,
    /// Play sounds when agents change state in background workspaces.
    pub sound: SoundConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AdvancedConfig {
    /// Allow launching herdr inside an existing herdr pane. Default: false.
    pub allow_nested: bool,
    /// Maximum scrollback buffer size in bytes retained per pane terminal. Default: 10000000.
    #[serde(alias = "scrollback_lines")]
    pub scrollback_limit_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SoundConfig {
    pub enabled: bool,
    /// Optional mp3 file path used for all notification sounds.
    /// Relative paths are resolved from the config file's directory.
    pub path: Option<PathBuf>,
    /// Optional mp3 file path for "done" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub done_path: Option<PathBuf>,
    /// Optional mp3 file path for "request" notifications.
    /// Relative paths are resolved from the config file's directory.
    pub request_path: Option<PathBuf>,
    pub agents: AgentSoundOverrides,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentSoundOverrides {
    pub pi: AgentSoundSetting,
    pub claude: AgentSoundSetting,
    pub codex: AgentSoundSetting,
    pub gemini: AgentSoundSetting,
    pub cursor: AgentSoundSetting,
    pub cline: AgentSoundSetting,
    pub open_code: AgentSoundSetting,
    pub github_copilot: AgentSoundSetting,
    pub kimi: AgentSoundSetting,
    pub droid: AgentSoundSetting,
    pub amp: AgentSoundSetting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSoundSetting {
    #[default]
    Default,
    On,
    Off,
}

impl SoundConfig {
    pub fn allows(&self, agent: Option<Agent>) -> bool {
        if !self.enabled {
            return false;
        }

        !matches!(self.agents.for_agent(agent), AgentSoundSetting::Off)
    }

    pub fn path_for(&self, sound: crate::sound::Sound) -> Option<PathBuf> {
        let path = match sound {
            crate::sound::Sound::Done => self.done_path.as_ref().or(self.path.as_ref()),
            crate::sound::Sound::Request => self.request_path.as_ref().or(self.path.as_ref()),
        }?;

        Some(resolve_config_relative_path(path))
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        for (field, path) in [
            ("ui.sound.path", self.path.as_ref()),
            ("ui.sound.done_path", self.done_path.as_ref()),
            ("ui.sound.request_path", self.request_path.as_ref()),
        ] {
            let Some(path) = path else {
                continue;
            };

            let resolved = resolve_config_relative_path(path);
            if resolved
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("mp3"))
            {
                diagnostics.push(format!(
                    "unsupported sound file format: {field} = {} resolves to {}; expected an mp3 file; using default sound",
                    path.display(),
                    resolved.display()
                ));
                continue;
            }

            if !resolved.exists() {
                diagnostics.push(format!(
                    "missing sound file: {field} = {} resolves to {}; using default sound",
                    path.display(),
                    resolved.display()
                ));
            } else if !resolved.is_file() {
                diagnostics.push(format!(
                    "invalid sound file: {field} = {} resolves to {}; using default sound",
                    path.display(),
                    resolved.display()
                ));
            }
        }
        diagnostics
    }
}

impl AgentSoundOverrides {
    pub fn for_agent(&self, agent: Option<Agent>) -> AgentSoundSetting {
        match agent {
            Some(Agent::Pi) => self.pi,
            Some(Agent::Claude) => self.claude,
            Some(Agent::Codex) => self.codex,
            Some(Agent::Gemini) => self.gemini,
            Some(Agent::Cursor) => self.cursor,
            Some(Agent::Cline) => self.cline,
            Some(Agent::OpenCode) => self.open_code,
            Some(Agent::GithubCopilot) => self.github_copilot,
            Some(Agent::Kimi) => self.kimi,
            Some(Agent::Droid) => self.droid,
            Some(Agent::Amp) => self.amp,
            None => AgentSoundSetting::Default,
        }
    }
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl+b".into(),
            new_workspace: "n".into(),
            rename_workspace: "shift+n".into(),
            close_workspace: "d".into(),
            previous_workspace: "".into(),
            next_workspace: "".into(),
            new_tab: "c".into(),
            rename_tab: "".into(),
            previous_tab: "".into(),
            next_tab: "".into(),
            close_tab: "".into(),
            focus_pane_left: "".into(),
            focus_pane_down: "".into(),
            focus_pane_up: "".into(),
            focus_pane_right: "".into(),
            split_vertical: "v".into(),
            split_horizontal: "-".into(),
            close_pane: "x".into(),
            fullscreen: "f".into(),
            resize_mode: "r".into(),
            toggle_sidebar: "b".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            confirm_close: true,
            accent: "cyan".into(),
            toast: ToastConfig::default(),
            sound: SoundConfig::default(),
        }
    }
}

impl Default for ToastConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

impl Default for SoundConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            done_path: None,
            request_path: None,
            agents: AgentSoundOverrides::default(),
        }
    }
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            allow_nested: false,
            scrollback_limit_bytes: DEFAULT_SCROLLBACK_LIMIT_BYTES,
        }
    }
}

impl Default for AgentSoundOverrides {
    fn default() -> Self {
        Self {
            pi: AgentSoundSetting::Default,
            claude: AgentSoundSetting::Default,
            codex: AgentSoundSetting::Default,
            gemini: AgentSoundSetting::Default,
            cursor: AgentSoundSetting::Default,
            cline: AgentSoundSetting::Default,
            open_code: AgentSoundSetting::Default,
            github_copilot: AgentSoundSetting::Default,
            kimi: AgentSoundSetting::Default,
            droid: AgentSoundSetting::Off,
            amp: AgentSoundSetting::Default,
        }
    }
}

impl Config {
    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
    }

    pub fn load() -> LoadedConfig {
        let path = config_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<Config>(&content) {
                    Ok(config) => {
                        let diagnostics = config.collect_diagnostics();
                        return LoadedConfig {
                            config,
                            diagnostics,
                        };
                    }
                    Err(e) => {
                        warn!(err = %e, "config parse error, using defaults");
                        return LoadedConfig {
                            config: Self::default(),
                            diagnostics: vec![format!("config parse error: {e}; using defaults")],
                        };
                    }
                },
                Err(e) => {
                    warn!(err = %e, "config read error, using defaults");
                    return LoadedConfig {
                        config: Self::default(),
                        diagnostics: vec![format!("config read error: {e}; using defaults")],
                    };
                }
            }
        }
        LoadedConfig {
            config: Self::default(),
            diagnostics: Vec::new(),
        }
    }

    pub fn prefix_key(&self) -> (KeyCode, KeyModifiers) {
        self.validated_keybinds().1
    }

    /// Parsed keybinds for navigate mode actions.
    pub fn keybinds(&self) -> Keybinds {
        self.validated_keybinds().3
    }

    pub fn collect_diagnostics(&self) -> Vec<String> {
        let (prefix_diag, _, keybind_diags, _) = self.validated_keybinds();
        prefix_diag
            .into_iter()
            .chain(keybind_diags)
            .chain(self.ui.sound.diagnostics())
            .collect()
    }
}

fn resolve_config_relative_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
}

impl Config {
    fn validated_keybinds(
        &self,
    ) -> (
        Option<String>,
        (KeyCode, KeyModifiers),
        Vec<String>,
        Keybinds,
    ) {
        #[derive(Clone)]
        struct Binding<'a> {
            field: &'a str,
            label: String,
            default_label: &'a str,
            value: (KeyCode, KeyModifiers),
            default: (KeyCode, KeyModifiers),
        }

        let mut diagnostics = Vec::new();
        let (prefix, prefix_diag) = parse_key_combo_with_diagnostic(
            &self.keys.prefix,
            "keys.prefix",
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        if let Some(diag) = &prefix_diag {
            warn!(message = %diag, "config diagnostic");
        }

        fn binding<'a>(
            field: &'a str,
            configured_label: &'a str,
            default_label: &'a str,
            default: (KeyCode, KeyModifiers),
            diagnostics: &mut Vec<String>,
        ) -> Binding<'a> {
            let (value, diag) = parse_key_combo_with_diagnostic(configured_label, field, default);
            let label = if let Some(diag) = diag {
                diagnostics.push(diag);
                default_label.to_string()
            } else {
                configured_label.to_string()
            };
            Binding {
                field,
                label,
                default_label,
                value,
                default,
            }
        }

        fn optional_binding(
            field: &'static str,
            configured_label: &str,
            diagnostics: &mut Vec<String>,
        ) -> (Option<(KeyCode, KeyModifiers)>, Option<String>) {
            if configured_label.trim().is_empty() {
                return (None, None);
            }
            let (value, diag) = parse_key_combo_with_diagnostic(
                configured_label,
                field,
                (KeyCode::Null, KeyModifiers::empty()),
            );
            if let Some(diag) = diag {
                diagnostics.push(diag);
                (None, None)
            } else {
                (Some(value), Some(configured_label.to_string()))
            }
        }

        let mut bindings = vec![
            binding(
                "keys.new_workspace",
                &self.keys.new_workspace,
                "n",
                (KeyCode::Char('n'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.rename_workspace",
                &self.keys.rename_workspace,
                "shift+n",
                (KeyCode::Char('n'), KeyModifiers::SHIFT),
                &mut diagnostics,
            ),
            binding(
                "keys.close_workspace",
                &self.keys.close_workspace,
                "d",
                (KeyCode::Char('d'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.new_tab",
                &self.keys.new_tab,
                "c",
                (KeyCode::Char('c'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.split_vertical",
                &self.keys.split_vertical,
                "v",
                (KeyCode::Char('v'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.split_horizontal",
                &self.keys.split_horizontal,
                "-",
                (KeyCode::Char('-'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.close_pane",
                &self.keys.close_pane,
                "x",
                (KeyCode::Char('x'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.fullscreen",
                &self.keys.fullscreen,
                "f",
                (KeyCode::Char('f'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.resize_mode",
                &self.keys.resize_mode,
                "r",
                (KeyCode::Char('r'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            binding(
                "keys.toggle_sidebar",
                &self.keys.toggle_sidebar,
                "b",
                (KeyCode::Char('b'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
        ];

        let optional_bindings = [
            optional_binding(
                "keys.previous_workspace",
                &self.keys.previous_workspace,
                &mut diagnostics,
            ),
            optional_binding(
                "keys.next_workspace",
                &self.keys.next_workspace,
                &mut diagnostics,
            ),
            optional_binding("keys.rename_tab", &self.keys.rename_tab, &mut diagnostics),
            optional_binding(
                "keys.previous_tab",
                &self.keys.previous_tab,
                &mut diagnostics,
            ),
            optional_binding("keys.next_tab", &self.keys.next_tab, &mut diagnostics),
            optional_binding("keys.close_tab", &self.keys.close_tab, &mut diagnostics),
            optional_binding(
                "keys.focus_pane_left",
                &self.keys.focus_pane_left,
                &mut diagnostics,
            ),
            optional_binding(
                "keys.focus_pane_down",
                &self.keys.focus_pane_down,
                &mut diagnostics,
            ),
            optional_binding(
                "keys.focus_pane_up",
                &self.keys.focus_pane_up,
                &mut diagnostics,
            ),
            optional_binding(
                "keys.focus_pane_right",
                &self.keys.focus_pane_right,
                &mut diagnostics,
            ),
        ];

        use std::collections::HashMap;
        let mut seen: HashMap<(KeyCode, KeyModifiers), &str> = HashMap::new();
        for binding in &mut bindings {
            if let Some(first_field) = seen.get(&binding.value) {
                let diag = format!(
                    "duplicate keybinding: {} conflicts with {}; using default {}",
                    binding.field, first_field, binding.default_label
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                binding.value = binding.default;
                binding.label = binding.default_label.to_string();
            } else {
                seen.insert(binding.value, binding.field);
            }
        }

        let keybinds = Keybinds {
            new_workspace: bindings[0].value,
            new_workspace_label: bindings[0].label.clone(),
            rename_workspace: bindings[1].value,
            rename_workspace_label: bindings[1].label.clone(),
            close_workspace: bindings[2].value,
            close_workspace_label: bindings[2].label.clone(),
            previous_workspace: optional_bindings[0].0,
            previous_workspace_label: optional_bindings[0].1.clone(),
            next_workspace: optional_bindings[1].0,
            next_workspace_label: optional_bindings[1].1.clone(),
            new_tab: bindings[3].value,
            new_tab_label: bindings[3].label.clone(),
            rename_tab: optional_bindings[2].0,
            rename_tab_label: optional_bindings[2].1.clone(),
            previous_tab: optional_bindings[3].0,
            previous_tab_label: optional_bindings[3].1.clone(),
            next_tab: optional_bindings[4].0,
            next_tab_label: optional_bindings[4].1.clone(),
            close_tab: optional_bindings[5].0,
            close_tab_label: optional_bindings[5].1.clone(),
            focus_pane_left: optional_bindings[6].0,
            focus_pane_left_label: optional_bindings[6].1.clone(),
            focus_pane_down: optional_bindings[7].0,
            focus_pane_down_label: optional_bindings[7].1.clone(),
            focus_pane_up: optional_bindings[8].0,
            focus_pane_up_label: optional_bindings[8].1.clone(),
            focus_pane_right: optional_bindings[9].0,
            focus_pane_right_label: optional_bindings[9].1.clone(),
            split_vertical: bindings[4].value,
            split_vertical_label: bindings[4].label.clone(),
            split_horizontal: bindings[5].value,
            split_horizontal_label: bindings[5].label.clone(),
            close_pane: bindings[6].value,
            close_pane_label: bindings[6].label.clone(),
            fullscreen: bindings[7].value,
            fullscreen_label: bindings[7].label.clone(),
            resize_mode: bindings[8].value,
            resize_mode_label: bindings[8].label.clone(),
            toggle_sidebar: bindings[9].value,
            toggle_sidebar_label: bindings[9].label.clone(),
        };

        (prefix_diag, prefix, diagnostics, keybinds)
    }
}

/// Parsed keybinds for navigate mode actions.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub new_workspace: (KeyCode, KeyModifiers),
    pub new_workspace_label: String,
    pub rename_workspace: (KeyCode, KeyModifiers),
    pub rename_workspace_label: String,
    pub close_workspace: (KeyCode, KeyModifiers),
    pub close_workspace_label: String,
    pub previous_workspace: Option<(KeyCode, KeyModifiers)>,
    pub previous_workspace_label: Option<String>,
    pub next_workspace: Option<(KeyCode, KeyModifiers)>,
    pub next_workspace_label: Option<String>,
    pub new_tab: (KeyCode, KeyModifiers),
    pub new_tab_label: String,
    pub rename_tab: Option<(KeyCode, KeyModifiers)>,
    pub rename_tab_label: Option<String>,
    pub previous_tab: Option<(KeyCode, KeyModifiers)>,
    pub previous_tab_label: Option<String>,
    pub next_tab: Option<(KeyCode, KeyModifiers)>,
    pub next_tab_label: Option<String>,
    pub close_tab: Option<(KeyCode, KeyModifiers)>,
    pub close_tab_label: Option<String>,
    pub focus_pane_left: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_left_label: Option<String>,
    pub focus_pane_down: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_down_label: Option<String>,
    pub focus_pane_up: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_up_label: Option<String>,
    pub focus_pane_right: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_right_label: Option<String>,
    pub split_vertical: (KeyCode, KeyModifiers),
    pub split_vertical_label: String,
    pub split_horizontal: (KeyCode, KeyModifiers),
    pub split_horizontal_label: String,
    pub close_pane: (KeyCode, KeyModifiers),
    pub close_pane_label: String,
    pub fullscreen: (KeyCode, KeyModifiers),
    pub fullscreen_label: String,
    pub resize_mode: (KeyCode, KeyModifiers),
    pub resize_mode_label: String,
    pub toggle_sidebar: (KeyCode, KeyModifiers),
    pub toggle_sidebar_label: String,
}

/// Parse a color string into a ratatui Color.
/// Supports: hex (#rrggbb, #rgb), named colors, rgb(r,g,b).
pub fn parse_color(s: &str) -> ratatui::style::Color {
    use ratatui::style::Color;
    let s = s.trim().to_lowercase();

    // Hex: #rrggbb or #rgb
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
        } else if hex.len() == 3 {
            let chars: Vec<u8> = hex
                .chars()
                .filter_map(|c| u8::from_str_radix(&c.to_string(), 16).ok())
                .collect();
            if chars.len() == 3 {
                return Color::Rgb(chars[0] * 17, chars[1] * 17, chars[2] * 17);
            }
        }
    }

    // rgb(r, g, b)
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                parts[0].trim().parse::<u8>(),
                parts[1].trim().parse::<u8>(),
                parts[2].trim().parse::<u8>(),
            ) {
                return Color::Rgb(r, g, b);
            }
        }
    }

    // Named colors
    match s.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        _ => {
            warn!(color = s, "unknown color, defaulting to cyan");
            Color::Cyan
        }
    }
}

pub fn save_onboarding_choices(sound_enabled: bool, toast_enabled: bool) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let content = upsert_top_level_bool(&content, "onboarding", false);
    let content = upsert_section_bool(&content, "ui.sound", "enabled", sound_enabled);
    let content = upsert_section_bool(&content, "ui.toast", "enabled", toast_enabled);
    std::fs::write(path, content)
}

pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var(CONFIG_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }
    config_dir().join("config.toml")
}

fn upsert_top_level_bool(content: &str, key: &str, value: bool) -> String {
    let replacement = format!("{key} = {value}");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        if trimmed.starts_with(&format!("{key} ")) || trimmed.starts_with(&format!("{key}=")) {
            *line = replacement.clone();
            return lines.join("\n") + "\n";
        }
    }

    if lines.is_empty() {
        format!("{replacement}\n")
    } else {
        format!("{replacement}\n{}\n", lines.join("\n").trim_end())
    }
}

/// Write a key = value pair in a TOML section (creates section if missing).
pub fn upsert_section_value(content: &str, section: &str, key: &str, value: &str) -> String {
    upsert_section_raw(content, section, key, value)
}

pub fn upsert_section_bool(content: &str, section: &str, key: &str, value: bool) -> String {
    upsert_section_raw(content, section, key, &value.to_string())
}

fn upsert_section_raw(content: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let assignment = format!("{key} = {value}");
    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;
    let mut found_section = false;
    let mut inserted = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed == header {
            found_section = true;
            result.push(line.to_string());
            i += 1;

            while i < lines.len() {
                let current = lines[i];
                let current_trimmed = current.trim();
                if current_trimmed.starts_with('[') && current_trimmed.ends_with(']') {
                    if !inserted {
                        result.push(assignment.clone());
                        inserted = true;
                    }
                    break;
                }

                if current_trimmed.starts_with(&format!("{key} "))
                    || current_trimmed.starts_with(&format!("{key}="))
                {
                    result.push(assignment.clone());
                    inserted = true;
                } else {
                    result.push(current.to_string());
                }
                i += 1;
            }

            continue;
        }

        result.push(line.to_string());
        i += 1;
    }

    if !found_section {
        if !result.is_empty() && !result.last().is_some_and(|line| line.trim().is_empty()) {
            result.push(String::new());
        }
        result.push(header);
        result.push(assignment);
    } else if found_section && !inserted {
        result.push(assignment);
    }

    result.join("\n") + "\n"
}

fn parse_key_combo(s: &str) -> Option<(KeyCode, KeyModifiers)> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_str: Option<&str> = None;

    for part in &parts {
        let trimmed = part.trim();
        match trimmed.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            _ if trimmed.is_empty() => return None,
            _ => {
                if key_str.is_some() {
                    return None;
                }
                key_str = Some(trimmed);
            }
        }
    }

    let key_str = key_str?;

    let lower = key_str.to_lowercase();
    let code = match lower.as_str() {
        "space" | " " => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        s if s.len() == 1 => {
            let ch = key_str.chars().next().unwrap();
            if ch.is_ascii_uppercase() {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Char(ch.to_ascii_lowercase())
            } else {
                KeyCode::Char(ch)
            }
        }
        s if s.starts_with('f') => s[1..].parse::<u8>().ok().map(KeyCode::F)?,
        _ => return None,
    };

    Some((code, modifiers))
}

pub fn format_key_combo(binding: (KeyCode, KeyModifiers)) -> String {
    let (code, modifiers) = binding;
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }

    let key = match code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "shift+tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        _ => format!("{:?}", code).to_lowercase(),
    };

    if matches!(code, KeyCode::BackTab) {
        return if parts.is_empty() {
            key
        } else {
            format!("{}+tab", parts.join("+"))
        };
    }

    parts.push(key);
    parts.join("+")
}

fn parse_key_combo_with_diagnostic(
    s: &str,
    field: &str,
    fallback: (KeyCode, KeyModifiers),
) -> ((KeyCode, KeyModifiers), Option<String>) {
    match parse_key_combo(s) {
        Some(binding) => (binding, None),
        None => {
            let diag = format!("invalid keybinding: {field} = {s:?}; using fallback");
            warn!(message = %diag, "config diagnostic");
            (fallback, Some(diag))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn parse_simple_char() {
        assert_eq!(
            parse_key_combo("v"),
            Some((KeyCode::Char('v'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_ctrl_combo() {
        assert_eq!(
            parse_key_combo("ctrl+b"),
            Some((KeyCode::Char('b'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn parse_special_key() {
        assert_eq!(
            parse_key_combo("enter"),
            Some((KeyCode::Enter, KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("tab"),
            Some((KeyCode::Tab, KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("esc"),
            Some((KeyCode::Esc, KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("left"),
            Some((KeyCode::Left, KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("alt+right"),
            Some((KeyCode::Right, KeyModifiers::ALT))
        );
    }

    #[test]
    fn parse_ctrl_shift() {
        assert_eq!(
            parse_key_combo("ctrl+shift+a"),
            Some((
                KeyCode::Char('a'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
    }

    #[test]
    fn parse_f_key() {
        assert_eq!(
            parse_key_combo("f5"),
            Some((KeyCode::F(5), KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_punctuation_key() {
        assert_eq!(
            parse_key_combo("ctrl+`"),
            Some((KeyCode::Char('`'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn uppercase_char_implies_shift() {
        assert_eq!(
            parse_key_combo("D"),
            Some((KeyCode::Char('d'), KeyModifiers::SHIFT))
        );
    }

    #[test]
    fn explicit_shift_and_uppercase_do_not_double_apply_shift() {
        assert_eq!(
            parse_key_combo("shift+D"),
            Some((KeyCode::Char('d'), KeyModifiers::SHIFT))
        );
    }

    #[test]
    fn invalid_keybinding_is_rejected() {
        assert_eq!(parse_key_combo("ctrl+foo+bar"), None);
        assert_eq!(parse_key_combo("ctrl+"), None);
    }

    #[test]
    fn default_keybinds_parse() {
        let config = Config::default();
        let kb = config.keybinds();
        assert_eq!(kb.new_workspace.0, KeyCode::Char('n'));
        assert_eq!(
            kb.rename_workspace,
            (KeyCode::Char('n'), KeyModifiers::SHIFT)
        );
        assert_eq!(kb.close_workspace.0, KeyCode::Char('d'));
        assert_eq!(kb.split_vertical.0, KeyCode::Char('v'));
        assert_eq!(kb.split_horizontal.0, KeyCode::Char('-'));
        assert_eq!(kb.close_pane.0, KeyCode::Char('x'));
        assert_eq!(kb.fullscreen.0, KeyCode::Char('f'));
        assert_eq!(kb.resize_mode.0, KeyCode::Char('r'));
        assert_eq!(kb.toggle_sidebar.0, KeyCode::Char('b'));
    }

    #[test]
    fn custom_keybinds_from_toml() {
        let toml = r#"
[keys]
prefix = "ctrl+a"
new_workspace = "c"
rename_workspace = "shift+r"
close_workspace = "ctrl+d"
split_vertical = "s"
split_horizontal = "shift+s"
close_pane = "ctrl+w"
fullscreen = "z"
resize_mode = "ctrl+r"
toggle_sidebar = "tab"
focus_pane_left = "alt+h"
focus_pane_right = "alt+right"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let (code, mods) = config.prefix_key();
        assert_eq!(code, KeyCode::Char('a'));
        assert_eq!(mods, KeyModifiers::CONTROL);

        let kb = config.keybinds();
        assert_eq!(
            kb.new_workspace,
            (KeyCode::Char('c'), KeyModifiers::empty())
        );
        assert_eq!(
            kb.rename_workspace,
            (KeyCode::Char('r'), KeyModifiers::SHIFT)
        );
        assert_eq!(
            kb.close_workspace,
            (KeyCode::Char('d'), KeyModifiers::CONTROL)
        );
        assert_eq!(kb.split_vertical.0, KeyCode::Char('s'));
        assert_eq!(
            kb.split_horizontal,
            (KeyCode::Char('s'), KeyModifiers::SHIFT)
        );
        assert_eq!(kb.close_pane, (KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(kb.fullscreen.0, KeyCode::Char('z'));
        assert_eq!(kb.resize_mode, (KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(kb.toggle_sidebar, (KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(
            kb.focus_pane_left,
            Some((KeyCode::Char('h'), KeyModifiers::ALT))
        );
        assert_eq!(
            kb.focus_pane_right,
            Some((KeyCode::Right, KeyModifiers::ALT))
        );
        assert_eq!(kb.focus_pane_down, None);
        assert_eq!(kb.focus_pane_up, None);
    }

    #[test]
    fn uppercase_keybind_from_toml_flows_into_shift_combo() {
        let toml = r#"
[keys]
split_horizontal = "D"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let kb = config.keybinds();
        assert_eq!(
            kb.split_horizontal,
            (KeyCode::Char('d'), KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn invalid_keybinding_produces_diagnostic_and_falls_back() {
        let toml = r#"
[keys]
rename_workspace = "wat"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(diagnostics
            .iter()
            .any(|d| d.contains("keys.rename_workspace")));
        assert_eq!(
            kb.rename_workspace,
            (KeyCode::Char('n'), KeyModifiers::SHIFT)
        );
        assert_eq!(kb.rename_workspace_label, "shift+n");
    }

    #[test]
    fn toast_config_parses() {
        let toml = r#"
[ui.toast]
enabled = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.toast.enabled);
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
    fn upsert_top_level_bool_replaces_existing_value() {
        let content = "onboarding = true\n[keys]\nprefix = \"ctrl+b\"\n";
        let updated = upsert_top_level_bool(content, "onboarding", false);
        assert!(updated.contains("onboarding = false"));
        assert!(!updated.contains("onboarding = true"));
    }

    #[test]
    fn upsert_section_bool_adds_missing_section() {
        let updated = upsert_section_bool("", "ui.toast", "enabled", true);
        assert!(updated.contains("[ui.toast]"));
        assert!(updated.contains("enabled = true"));
    }

    #[test]
    fn duplicate_keybinding_produces_diagnostic_and_falls_back_later_binding() {
        let toml = r#"
[keys]
new_workspace = "g"
rename_workspace = "g"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();

        assert!(diagnostics
            .iter()
            .any(|d| d.contains("duplicate keybinding")));
        assert_eq!(
            kb.new_workspace,
            (KeyCode::Char('g'), KeyModifiers::empty())
        );
        assert_eq!(
            kb.rename_workspace,
            (KeyCode::Char('n'), KeyModifiers::SHIFT)
        );
        assert_eq!(kb.rename_workspace_label, "shift+n");
    }

    #[test]
    fn sound_table_config_parses() {
        let toml = r#"
[ui.sound]
enabled = true
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
request_path = "/tmp/request.mp3"

[ui.sound.agents]
droid = "off"
claude = "on"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.ui.sound.enabled);
        assert_eq!(config.ui.sound.path, Some(PathBuf::from("sounds/all.mp3")));
        assert_eq!(
            config.ui.sound.done_path,
            Some(PathBuf::from("sounds/done.mp3"))
        );
        assert_eq!(
            config.ui.sound.request_path,
            Some(PathBuf::from("/tmp/request.mp3"))
        );
        assert_eq!(config.ui.sound.agents.droid, AgentSoundSetting::Off);
        assert_eq!(config.ui.sound.agents.claude, AgentSoundSetting::On);
        assert_eq!(config.ui.sound.agents.pi, AgentSoundSetting::Default);
    }

    #[test]
    fn sound_path_resolution_prefers_specific_over_global() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/all.mp3"
done_path = "sounds/done.mp3"
"#,
        )
        .unwrap();

        let config_root = config_path().parent().unwrap().to_path_buf();
        assert_eq!(
            config.ui.sound.path_for(crate::sound::Sound::Done),
            Some(config_root.join("sounds/done.mp3"))
        );
        assert_eq!(
            config.ui.sound.path_for(crate::sound::Sound::Request),
            Some(config_root.join("sounds/all.mp3"))
        );
    }

    #[test]
    fn missing_sound_file_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
done_path = "sounds/missing.mp3"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(
            |diag| diag.contains("ui.sound.done_path") && diag.contains("using default sound")
        ));
    }

    #[test]
    fn non_mp3_sound_file_produces_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[ui.sound]
path = "sounds/notification.wav"
"#,
        )
        .unwrap();

        let diagnostics = config.collect_diagnostics();
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("ui.sound.path") && diag.contains("expected an mp3 file")
        }));
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
    fn advanced_config_parses() {
        let toml = r#"
[advanced]
allow_nested = true
scrollback_limit_bytes = 12345
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.advanced.allow_nested);
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

    #[test]
    fn theme_name_parses() {
        let toml = r#"
[theme]
name = "dracula"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.theme.name.as_deref(), Some("dracula"));
    }

    #[test]
    fn theme_custom_overrides_parse() {
        let toml = r##"
[theme]
name = "nord"

[theme.custom]
panel_bg = "#1e1e2e"
accent = "#ff79c6"
red = "rgb(255, 85, 85)"
"##;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.theme.name.as_deref(), Some("nord"));
        let custom = config.theme.custom.as_ref().unwrap();
        assert_eq!(custom.panel_bg.as_deref(), Some("#1e1e2e"));
        assert_eq!(custom.accent.as_deref(), Some("#ff79c6"));
        assert_eq!(custom.red.as_deref(), Some("rgb(255, 85, 85)"));
        assert!(custom.green.is_none());
    }

    #[test]
    fn theme_defaults_when_missing() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.theme.name.is_none());
        assert!(config.theme.custom.is_none());
    }
}
