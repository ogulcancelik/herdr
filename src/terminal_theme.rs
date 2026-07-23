#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAppearance {
    Dark,
    Light,
}

impl RgbColor {
    pub fn inferred_appearance(self) -> HostAppearance {
        let luminance = u32::from(self.r) * 299 + u32::from(self.g) * 587 + u32::from(self.b) * 114;
        if luminance >= 128_000 {
            HostAppearance::Light
        } else {
            HostAppearance::Dark
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalTheme {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
    pub palette: [Option<RgbColor>; 256],
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self {
            foreground: None,
            background: None,
            palette: [None; 256],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultColorKind {
    Foreground,
    Background,
}

pub const HOST_COLOR_QUERY_SEQUENCE: &str = "\x1b]10;?\x1b\\\x1b]11;?\x1b\\";
pub const HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE: &str = "\x1b[?2031h";
pub const HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE: &str = "\x1b[?2031l";

impl TerminalTheme {
    pub fn with_color(mut self, kind: DefaultColorKind, color: RgbColor) -> Self {
        match kind {
            DefaultColorKind::Foreground => self.foreground = Some(color),
            DefaultColorKind::Background => self.background = Some(color),
        }
        self
    }

    pub fn with_palette_color(mut self, index: u8, color: RgbColor) -> Self {
        self.palette[usize::from(index)] = Some(color);
        self
    }

    pub fn is_empty(self) -> bool {
        self.foreground.is_none() && self.background.is_none()
    }
}

pub fn host_terminal_theme_query_sequence() -> String {
    use std::fmt::Write as _;

    let mut sequence = String::from(HOST_COLOR_QUERY_SEQUENCE);
    for index in 0..=u8::MAX {
        let _ = write!(sequence, "\x1b]4;{index};?\x1b\\");
    }
    sequence
}

pub fn parse_default_color_response(sequence: &str) -> Option<(DefaultColorKind, RgbColor)> {
    let body = sequence.strip_prefix("\x1b]")?;
    let body = body
        .strip_suffix("\x1b\\")
        .or_else(|| body.strip_suffix('\u{7}'))?;
    let (command, value) = body.split_once(';')?;
    let kind = match command {
        "10" => DefaultColorKind::Foreground,
        "11" => DefaultColorKind::Background,
        _ => return None,
    };
    Some((kind, parse_rgb_color(value)?))
}

pub fn parse_palette_color_response(sequence: &str) -> Option<(u8, RgbColor)> {
    let body = sequence.strip_prefix("\x1b]4;")?;
    let body = body
        .strip_suffix("\x1b\\")
        .or_else(|| body.strip_suffix('\u{7}'))?;
    let (index, value) = body.split_once(';')?;
    Some((index.parse().ok()?, parse_rgb_color(value)?))
}

pub fn osc_set_default_color_sequence(kind: DefaultColorKind, color: RgbColor) -> String {
    let command = match kind {
        DefaultColorKind::Foreground => 10,
        DefaultColorKind::Background => 11,
    };
    format!(
        "\x1b]{command};rgb:{:02x}/{:02x}/{:02x}\x1b\\",
        color.r, color.g, color.b
    )
}

pub fn osc_reset_default_color_sequence(kind: DefaultColorKind) -> &'static str {
    match kind {
        DefaultColorKind::Foreground => "\x1b]110\x1b\\",
        DefaultColorKind::Background => "\x1b]111\x1b\\",
    }
}

fn parse_rgb_color(value: &str) -> Option<RgbColor> {
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let mut parts = rgb.split('/');
        return Some(RgbColor {
            r: parse_hex_component(parts.next()?)?,
            g: parse_hex_component(parts.next()?)?,
            b: parse_hex_component(parts.next()?)?,
        })
        .filter(|_| parts.next().is_none());
    }

    if let Some(hex) = value.strip_prefix('#') {
        let digits = hex.len() / 3;
        if !matches!(digits, 1..=4) || hex.len() != digits * 3 {
            return None;
        }
        return Some(RgbColor {
            r: parse_hex_component(&hex[..digits])?,
            g: parse_hex_component(&hex[digits..digits * 2])?,
            b: parse_hex_component(&hex[digits * 2..])?,
        });
    }

    None
}

fn parse_hex_component(component: &str) -> Option<u8> {
    if component.is_empty()
        || component.len() > 4
        || !component.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Some(((value * 255 + (max / 2)) / max) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_st_terminated_rgb_response() {
        let parsed = parse_default_color_response("\x1b]10;rgb:cccc/dddd/eeee\x1b\\");
        assert_eq!(
            parsed,
            Some((
                DefaultColorKind::Foreground,
                RgbColor {
                    r: 0xcc,
                    g: 0xdd,
                    b: 0xee,
                },
            ))
        );
    }

    #[test]
    fn parses_bel_terminated_hash_response() {
        let parsed = parse_default_color_response("\x1b]11;#123456\u{7}");
        assert_eq!(
            parsed,
            Some((
                DefaultColorKind::Background,
                RgbColor {
                    r: 0x12,
                    g: 0x34,
                    b: 0x56,
                },
            ))
        );
    }

    #[test]
    fn parses_palette_responses_and_builds_full_query() {
        assert_eq!(
            parse_palette_color_response("\x1b]4;255;rgb:1111/2222/3333\x1b\\"),
            Some((
                255,
                RgbColor {
                    r: 0x11,
                    g: 0x22,
                    b: 0x33,
                }
            ))
        );

        let query = host_terminal_theme_query_sequence();
        assert!(query.starts_with(HOST_COLOR_QUERY_SEQUENCE));
        assert!(query.contains("\x1b]4;0;?\x1b\\"));
        assert!(query.ends_with("\x1b]4;255;?\x1b\\"));
        assert_eq!(query.matches("\x1b]4;").count(), 256);
    }

    #[test]
    fn default_color_reset_sequences_use_xterm_osc_numbers() {
        assert_eq!(
            osc_reset_default_color_sequence(DefaultColorKind::Foreground),
            "\x1b]110\x1b\\"
        );
        assert_eq!(
            osc_reset_default_color_sequence(DefaultColorKind::Background),
            "\x1b]111\x1b\\"
        );
    }

    #[test]
    fn scales_short_hex_components() {
        assert_eq!(parse_hex_component("f"), Some(255));
        assert_eq!(parse_hex_component("80"), Some(128));
        assert_eq!(parse_hex_component("800"), Some(128));
        assert_eq!(parse_hex_component("8000"), Some(128));
    }
}
