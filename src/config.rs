use crossterm::event::{KeyCode, KeyModifiers};

mod io;
mod keybinds;
mod model;
mod sound;
mod theme;

pub use self::{
    io::{
        config_diagnostic_summary, config_dir, config_path, load_live_config,
        remove_keybinding_config_sections, remove_section_key, state_dir, upsert_section_bool,
        upsert_section_value,
    },
    keybinds::{
        format_key_combo, normalize_key_combo, terminal_key_matches_combo, ActionKeybinds,
        BindingConfig, CommandKeybindConfig, CustomCommandAction, CustomCommandKeybind,
        IndexedKeybind, Keybinds, LiveKeybindConfig,
    },
    model::{
        validated_sidebar_bounds, AgentPanelScopeConfig, Config, ConfigReloadReport,
        ConfigReloadStatus, KeysConfig, NewTerminalCwdConfig, ShellModeConfig, ToastConfig,
        ToastDelivery,
    },
    sound::SoundConfig,
    theme::{parse_color, CustomThemeColors, ThemeConfig},
};

pub(crate) use self::io::upsert_top_level_bool;

pub const CONFIG_PATH_ENV_VAR: &str = "HERDR_CONFIG_PATH";
pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10_000_000;
pub const DEFAULT_MOUSE_SCROLL_LINES: usize = 3;
pub const DEFAULT_MOBILE_WIDTH_THRESHOLD: u16 = 64;

#[cfg(test)]
pub(crate) fn app_dir_name() -> &'static str {
    io::app_dir_name()
}

#[cfg(test)]
pub(crate) fn test_config_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

impl Config {
    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
    }

    pub fn prefix_key(&self) -> (KeyCode, KeyModifiers) {
        self.validated_keybinds().1
    }

    /// Parsed keybinds for Herdr actions.
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

    pub(crate) fn local_keybindings_profile_toml(&self) -> Result<String, toml::ser::Error> {
        let mut keys = self.keys.clone();
        keys.command.clear();

        #[derive(serde::Serialize)]
        struct KeysProfile {
            keys: KeysConfig,
        }

        toml::to_string_pretty(&KeysProfile { keys })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_keybindings_profile_includes_defaults_and_excludes_commands() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "lazygit"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        assert!(profile.contains("[keys]"));
        assert!(profile.contains("prefix = \"ctrl+a\""));
        assert!(profile.contains("new_tab = \"prefix+t\""));
        assert!(profile.contains("next_tab = \"prefix+n\""));
        assert!(!profile.contains("lazygit"));
        assert!(!profile.contains("command ="));
        assert!(!profile.contains("[[keys.command]]"));
    }
}
