use crossterm::event::{KeyCode, KeyModifiers};

mod io;
mod keybinds;
mod model;
mod sound;
mod theme;

pub use self::{
    io::{
        config_dir, config_path, load_live_keybinds, save_onboarding_choices, upsert_section_bool,
        upsert_section_value,
    },
    keybinds::{
        format_key_combo, CommandKeybindConfig, CustomCommandAction, CustomCommandKeybind,
        Keybinds, LiveKeybindConfig,
    },
    model::{Config, ToastConfig},
    sound::{AgentSoundSetting, SoundConfig},
    theme::{parse_color, CustomThemeColors, ThemeConfig},
};

pub const CONFIG_PATH_ENV_VAR: &str = "HERDR_CONFIG_PATH";
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;

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

#[derive(Debug)]
pub struct LiveKeybindConfig {
    pub prefix: (KeyCode, KeyModifiers),
    pub keybinds: Keybinds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandKeybindType {
    #[default]
    Shell,
    Pane,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommandKeybindConfig {
    /// Navigate-mode key that runs a command after pressing the prefix key.
    pub key: String,
    /// Command executed either in the background shell or inside a pane.
    pub command: String,
    /// Command execution mode. Default: "shell".
    #[serde(rename = "type")]
    pub action_type: CommandKeybindType,
}

impl Default for CommandKeybindConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            command: String::new(),
            action_type: CommandKeybindType::Shell,
        }
    }
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
    /// Prefix-mode custom command bindings.
    pub command: Vec<CommandKeybindConfig>,
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
            Some(Agent::Hermes) => AgentSoundSetting::Default,
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
            command: Vec::new(),
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

#[cfg(test)]
pub(crate) fn app_dir_name() -> &'static str {
    io::app_dir_name()
}

impl Config {
    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
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

    pub fn live_keybinds(&self) -> Result<LiveKeybindConfig, Vec<String>> {
        let (prefix_diag, prefix, keybind_diags, keybinds) = self.validated_keybinds();
        let diagnostics: Vec<String> = prefix_diag.into_iter().chain(keybind_diags).collect();
        if diagnostics.is_empty() {
            Ok(LiveKeybindConfig { prefix, keybinds })
        } else {
            Err(diagnostics)
        }
    }
}
