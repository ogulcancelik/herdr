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
        validated_sidebar_bounds, AgentPanelSortConfig, Config, ConfigReloadReport,
        ConfigReloadStatus, HostCursorModeConfig, NewTerminalCwdConfig, ShellModeConfig,
        SidebarCollapsedModeConfig, ToastClipboardPosition, ToastConfig, ToastDelivery,
        ToastHerdrPosition, UpdateChannelConfig, MAX_TOAST_DELAY_SECONDS,
    },
    sound::SoundConfig,
    theme::{parse_color, CustomThemeColors, ThemeConfig},
};

pub(crate) use self::io::upsert_top_level_bool;
pub(crate) use self::keybinds::parse_key_combo;

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
            .chain(self.remote_image_paste_key().err())
            .chain(self.ui.sound.diagnostics())
            .collect()
    }

    pub(crate) fn remote_image_paste_key(&self) -> Result<Option<(KeyCode, KeyModifiers)>, String> {
        let raw = self.keys.remote_image_paste.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        parse_key_combo(raw).map(Some).ok_or_else(|| {
            format!("invalid keybinding: keys.remote_image_paste = {raw:?}; disabling binding")
        })
    }

    #[cfg(test)]
    pub fn live_keybinds(&self) -> Result<LiveKeybindConfig, Vec<String>> {
        self.live_keybinds_with_diagnostics()
            .map(|(live, _diagnostics)| live)
    }

    pub(crate) fn live_keybinds_with_diagnostics(
        &self,
    ) -> Result<(LiveKeybindConfig, Vec<String>), Vec<String>> {
        let (prefix_diag, prefix, keybind_diags, keybinds) = self.validated_keybinds();
        if let Some(prefix_diag) = prefix_diag {
            Err(std::iter::once(prefix_diag).chain(keybind_diags).collect())
        } else {
            Ok((LiveKeybindConfig { prefix, keybinds }, keybind_diags))
        }
    }

    pub(crate) fn local_keybindings_profile_toml(&self) -> Result<String, toml::ser::Error> {
        #[derive(serde::Serialize)]
        struct KeysProfile {
            keys: model::KeysConfigOverlay,
        }

        toml::to_string_pretty(&KeysProfile {
            keys: self.keys.local_profile(&self.keybinds()),
        })
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

    #[test]
    fn local_keybindings_profile_preserves_user_default_provenance() {
        let config: Config = toml::from_str(
            r#"
[keys]
zoom = "prefix+?"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("zoom = \"prefix+?\""));
        assert!(!profile.contains("help = \"prefix+?\""));
        assert!(round_tripped
            .keybinds()
            .zoom
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+?"));
        assert!(round_tripped.keybinds().help.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_omits_default_displaced_by_user_prefix() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "n"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("prefix = \"n\""));
        assert!(!profile.contains("next_tab = \"prefix+n\""));
        assert!(round_tripped.keybinds().next_tab.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_preserves_legacy_indexed_tab_source() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
tabs = "ctrl"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();
        let keybinds = round_tripped.keybinds();
        let switch_tab_labels: Vec<_> = keybinds
            .switch_tab
            .iter()
            .map(|binding| binding.label.as_str())
            .collect();

        assert!(profile.contains("[keys.indexed]"));
        assert!(profile.contains("tabs = \"ctrl\""));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert_eq!(switch_tab_labels.len(), 9);
        assert!(switch_tab_labels
            .iter()
            .all(|label| label.starts_with("ctrl+")));
    }

    #[test]
    fn local_keybindings_profile_keeps_invalid_legacy_indexed_default_disabled() {
        let config: Config = toml::from_str(
            r#"
[keys.indexed]
tabs = "bogus"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(profile.contains("[keys.indexed]"));
        assert!(profile.contains("tabs = \"bogus\""));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert!(round_tripped.keybinds().switch_tab.is_empty());
    }

    #[test]
    fn local_keybindings_profile_keeps_default_displaced_by_omitted_command_disabled() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+n"
command = "echo next"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();

        assert!(!profile.contains("[[keys.command]]"));
        assert!(!profile.contains("command ="));
        assert!(profile.contains("next_tab = \"\""));
        assert!(round_tripped.keybinds().next_tab.bindings.is_empty());
    }

    #[test]
    fn local_keybindings_profile_preserves_partially_displaced_indexed_default() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+1"
command = "echo one"
"#,
        )
        .unwrap();

        let profile = config.local_keybindings_profile_toml().unwrap();
        let round_tripped: Config = toml::from_str(&profile).unwrap();
        let keybinds = round_tripped.keybinds();
        let switch_tab_labels: Vec<_> = keybinds
            .switch_tab
            .iter()
            .map(|binding| binding.label.as_str())
            .collect();

        assert!(!profile.contains("[[keys.command]]"));
        assert!(!profile.contains("switch_tab = \"prefix+1..9\""));
        assert!(profile.contains("\"prefix+2\""));
        assert!(profile.contains("\"prefix+9\""));
        assert!(!switch_tab_labels.contains(&"prefix+1"));
        assert_eq!(switch_tab_labels.len(), 8);
        assert!(switch_tab_labels
            .iter()
            .all(|label| label.starts_with("prefix+")));
    }

    #[test]
    fn remote_image_paste_key_defaults_to_ctrl_v() {
        let config = Config::default();
        assert_eq!(
            config.remote_image_paste_key().unwrap(),
            Some((KeyCode::Char('v'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn remote_image_paste_key_can_be_disabled() {
        let config: Config = toml::from_str("[keys]\nremote_image_paste = ''\n").unwrap();
        assert_eq!(config.remote_image_paste_key().unwrap(), None);
    }

    #[test]
    fn ui_host_cursor_defaults_to_auto_and_parses_overrides() {
        let default_config = Config::default();
        assert_eq!(default_config.ui.host_cursor, HostCursorModeConfig::Auto);

        let native: Config = toml::from_str("[ui]\nhost_cursor = 'native'\n").unwrap();
        assert_eq!(native.ui.host_cursor, HostCursorModeConfig::Native);

        let drawn: Config = toml::from_str("[ui]\nhost_cursor = 'drawn'\n").unwrap();
        assert_eq!(drawn.ui.host_cursor, HostCursorModeConfig::Drawn);
    }
}
