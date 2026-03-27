use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use tracing::warn;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub keys: KeysConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Prefix key to toggle navigate mode (e.g. "ctrl+s", "ctrl+a", "ctrl+b").
    pub prefix: String,
    /// Split pane vertically (side by side). Default: "v"
    pub split_vertical: String,
    /// Split pane horizontally (stacked). Default: "-"
    pub split_horizontal: String,
    /// Close the focused pane. Default: "x"
    pub close_pane: String,
    /// Toggle fullscreen for the focused pane. Default: "f"
    pub fullscreen: String,
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
    /// Play sounds when agents change state in background workspaces.
    pub sound: bool,
}

impl Default for KeysConfig {
    fn default() -> Self {
        Self {
            prefix: "ctrl+s".into(),
            split_vertical: "v".into(),
            split_horizontal: "-".into(),
            close_pane: "x".into(),
            fullscreen: "f".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            confirm_close: true,
            accent: "cyan".into(),
            sound: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => warn!(err = %e, "config parse error, using defaults"),
                },
                Err(e) => warn!(err = %e, "config read error, using defaults"),
            }
        }
        Self::default()
    }

    pub fn prefix_key(&self) -> (KeyCode, KeyModifiers) {
        parse_key_combo(&self.keys.prefix)
    }

    /// Human-readable label for the prefix key (shown in status bar).
    pub fn prefix_label(&self) -> String {
        self.keys.prefix.clone()
    }

    /// Parsed keybinds for navigate mode actions.
    pub fn keybinds(&self) -> Keybinds {
        Keybinds {
            split_vertical: parse_key_combo(&self.keys.split_vertical),
            split_horizontal: parse_key_combo(&self.keys.split_horizontal),
            close_pane: parse_key_combo(&self.keys.close_pane),
            fullscreen: parse_key_combo(&self.keys.fullscreen),
        }
    }
}

/// Parsed keybinds for navigate mode actions.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub split_vertical: (KeyCode, KeyModifiers),
    pub split_horizontal: (KeyCode, KeyModifiers),
    pub close_pane: (KeyCode, KeyModifiers),
    pub fullscreen: (KeyCode, KeyModifiers),
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

fn config_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("herdr/config.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/herdr/config.toml")
    } else {
        PathBuf::from("/tmp/herdr/config.toml")
    }
}

fn parse_key_combo(s: &str) -> (KeyCode, KeyModifiers) {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_str = "";

    for part in &parts {
        match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            _ => key_str = part.trim(),
        }
    }

    let code = match key_str.to_lowercase().as_str() {
        "space" | " " => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        s if s.len() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        s if s.starts_with('f') => s[1..]
            .parse::<u8>()
            .map(KeyCode::F)
            .unwrap_or(KeyCode::Char('s')),
        _ => KeyCode::Char('s'),
    };

    (code, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn parse_simple_char() {
        assert_eq!(parse_key_combo("v"), (KeyCode::Char('v'), KeyModifiers::empty()));
    }

    #[test]
    fn parse_ctrl_combo() {
        assert_eq!(parse_key_combo("ctrl+s"), (KeyCode::Char('s'), KeyModifiers::CONTROL));
    }

    #[test]
    fn parse_special_key() {
        assert_eq!(parse_key_combo("enter"), (KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(parse_key_combo("tab"), (KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(parse_key_combo("esc"), (KeyCode::Esc, KeyModifiers::empty()));
    }

    #[test]
    fn parse_ctrl_shift() {
        assert_eq!(
            parse_key_combo("ctrl+shift+a"),
            (KeyCode::Char('a'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn parse_f_key() {
        assert_eq!(parse_key_combo("f5"), (KeyCode::F(5), KeyModifiers::empty()));
    }

    #[test]
    fn default_keybinds_parse() {
        let config = Config::default();
        let kb = config.keybinds();
        assert_eq!(kb.split_vertical.0, KeyCode::Char('v'));
        assert_eq!(kb.split_horizontal.0, KeyCode::Char('-'));
        assert_eq!(kb.close_pane.0, KeyCode::Char('x'));
        assert_eq!(kb.fullscreen.0, KeyCode::Char('f'));
    }

    #[test]
    fn custom_keybinds_from_toml() {
        let toml = r#"
[keys]
prefix = "ctrl+a"
split_vertical = "s"
split_horizontal = "shift+s"
close_pane = "ctrl+w"
fullscreen = "z"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let (code, mods) = config.prefix_key();
        assert_eq!(code, KeyCode::Char('a'));
        assert_eq!(mods, KeyModifiers::CONTROL);

        let kb = config.keybinds();
        assert_eq!(kb.split_vertical.0, KeyCode::Char('s'));
        assert_eq!(kb.split_horizontal, (KeyCode::Char('s'), KeyModifiers::SHIFT));
        assert_eq!(kb.close_pane, (KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(kb.fullscreen.0, KeyCode::Char('z'));
    }
}
