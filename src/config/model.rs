use serde::{Deserialize, Deserializer, Serialize};

use super::{CommandKeybindConfig, SoundConfig, ThemeConfig, DEFAULT_SCROLLBACK_LIMIT_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToastDelivery {
    #[default]
    Off,
    Herdr,
    Terminal,
}

#[derive(Debug, Clone)]
pub struct ToastConfig {
    pub delivery: ToastDelivery,
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

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub theme: ThemeConfig,
    pub keys: KeysConfig,
    pub ui: UiConfig,
    pub advanced: AdvancedConfig,
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub diagnostics: Vec<String>,
    pub invalid_sections: Vec<String>,
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
    /// Close the selected workspace. Default: "shift+d"
    pub close_workspace: String,
    /// Optional explicit detach shortcut in server/client mode. Unset by default.
    pub detach: String,
    /// Reload config.toml in the running app/server. Unset by default.
    pub reload_config: String,
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

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl+b".into(),
            new_workspace: "n".into(),
            rename_workspace: "shift+n".into(),
            close_workspace: "shift+d".into(),
            detach: "".into(),
            reload_config: "".into(),
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
        let delivery = raw.delivery.unwrap_or_else(|| match raw.enabled {
            Some(true) => ToastDelivery::Herdr,
            Some(false) => ToastDelivery::Off,
            None => ToastDelivery::Off,
        });
        Ok(Self { delivery })
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
