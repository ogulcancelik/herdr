use std::path::PathBuf;

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
    assert_eq!(
        kb.close_workspace,
        (KeyCode::Char('d'), KeyModifiers::SHIFT)
    );
    assert_eq!(kb.detach, None);
    assert_eq!(kb.split_vertical.0, KeyCode::Char('v'));
    assert_eq!(kb.split_horizontal.0, KeyCode::Char('-'));
    assert_eq!(kb.close_pane.0, KeyCode::Char('x'));
    assert_eq!(kb.fullscreen.0, KeyCode::Char('f'));
    assert_eq!(kb.resize_mode.0, KeyCode::Char('r'));
    assert_eq!(kb.toggle_sidebar.0, KeyCode::Char('b'));
    assert!(kb.custom_commands.is_empty());
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
close_pane = "X"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let kb = config.keybinds();
    assert_eq!(kb.close_pane, (KeyCode::Char('x'), KeyModifiers::SHIFT));
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
fn duplicate_optional_keybinding_is_disabled_with_diagnostic() {
    let toml = r#"
[keys]
new_workspace = "g"
rename_tab = "g"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let diagnostics = config.collect_diagnostics();
    let kb = config.keybinds();

    assert!(diagnostics
        .iter()
        .any(|d| d.contains("keys.rename_tab") && d.contains("disabling binding")));
    assert_eq!(
        kb.new_workspace,
        (KeyCode::Char('g'), KeyModifiers::empty())
    );
    assert_eq!(kb.rename_tab, None);
}

#[test]
fn custom_command_keybinds_parse_from_toml() {
    let toml = r#"
[[keys.command]]
key = "g"
command = "echo hi"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let kb = config.keybinds();

    assert_eq!(kb.custom_commands.len(), 1);
    assert_eq!(
        kb.custom_commands[0].key,
        (KeyCode::Char('g'), KeyModifiers::empty())
    );
    assert_eq!(kb.custom_commands[0].label, "g");
    assert_eq!(kb.custom_commands[0].command, "echo hi");
    assert_eq!(kb.custom_commands[0].action, CustomCommandAction::Shell);
}

#[test]
fn pane_custom_command_keybinds_parse_from_toml() {
    let toml = r#"
[[keys.command]]
key = "g"
type = "pane"
command = "lazygit"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let kb = config.keybinds();

    assert_eq!(kb.custom_commands.len(), 1);
    assert_eq!(kb.custom_commands[0].action, CustomCommandAction::Pane);
}

#[test]
fn custom_command_conflicting_with_builtin_is_disabled_with_diagnostic() {
    let toml = r#"
[keys]
new_workspace = "g"

[[keys.command]]
key = "g"
command = "echo hi"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let diagnostics = config.collect_diagnostics();
    let kb = config.keybinds();

    assert!(diagnostics.iter().any(|d| {
        d.contains("duplicate custom keybinding")
            && d.contains("keys.command[0].key")
            && d.contains("keys.new_workspace")
    }));
    assert!(kb.custom_commands.is_empty());
}

#[test]
fn custom_command_conflicting_with_reserved_navigate_key_is_disabled_with_diagnostic() {
    let toml = r#"
[[keys.command]]
key = "q"
command = "echo hi"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let diagnostics = config.collect_diagnostics();
    let kb = config.keybinds();

    assert!(diagnostics.iter().any(|d| {
        d.contains("duplicate custom keybinding")
            && d.contains("keys.command[0].key")
            && d.contains("navigate.quit")
    }));
    assert!(kb.custom_commands.is_empty());
}

#[test]
fn live_keybinds_reject_invalid_keybinding() {
    let config: Config = toml::from_str(
        r#"
[keys]
rename_workspace = "wat"
"#,
    )
    .unwrap();

    let diagnostics = config.live_keybinds().unwrap_err();
    assert!(diagnostics
        .iter()
        .any(|d| d.contains("keys.rename_workspace")));
}

#[test]
fn live_keybinds_ignore_non_key_diagnostics() {
    let config: Config = toml::from_str(
        r#"
[keys]
new_workspace = "g"

[ui.sound]
done_path = "sounds/missing.mp3"
"#,
    )
    .unwrap();

    let live = config.live_keybinds().unwrap();
    assert_eq!(
        live.keybinds.new_workspace,
        (KeyCode::Char('g'), KeyModifiers::empty())
    );
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
    assert!(diagnostics
        .iter()
        .any(|diag| diag.contains("ui.sound.done_path") && diag.contains("using default sound")));
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
    assert!(diagnostics
        .iter()
        .any(|diag| { diag.contains("ui.sound.path") && diag.contains("expected an mp3 file") }));
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
fn parse_color_accepts_reset_aliases() {
    use ratatui::style::Color;

    for value in ["reset", "default", "none", "transparent"] {
        assert_eq!(parse_color(value), Color::Reset, "value: {value}");
    }
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
