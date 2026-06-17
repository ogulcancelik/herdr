#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalTheme {
    pub foreground: Option<RgbColor>,
    pub background: Option<RgbColor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultColorKind {
    Foreground,
    Background,
}

pub const HOST_COLOR_QUERY_SEQUENCE: &str = "\x1b]10;?\x1b\\\x1b]11;?\x1b\\";

/// Number of ANSI palette entries (0..16) Herdr queries from the host terminal
/// so it can resolve indexed colors to real RGB (e.g. when dimming panes).
pub const HOST_ANSI_PALETTE_LEN: usize = 16;

/// The host terminal's 16 ANSI palette colors, as reported via OSC 4.
pub type AnsiPalette = [Option<RgbColor>; HOST_ANSI_PALETTE_LEN];

/// OSC 4 queries for the 16 ANSI palette colors. Sent alongside the OSC 10/11
/// default-color queries; responses arrive on the same input stream and are
/// parsed by `parse_palette_color_response`.
pub fn host_palette_query_sequence() -> &'static str {
    use std::sync::OnceLock;
    static SEQ: OnceLock<String> = OnceLock::new();
    SEQ.get_or_init(|| {
        let mut sequence = String::with_capacity(HOST_ANSI_PALETTE_LEN * 12);
        for index in 0..HOST_ANSI_PALETTE_LEN {
            sequence.push_str(&format!("\x1b]4;{index};?\x1b\\"));
        }
        sequence
    })
    .as_str()
}

impl TerminalTheme {
    pub fn with_color(mut self, kind: DefaultColorKind, color: RgbColor) -> Self {
        match kind {
            DefaultColorKind::Foreground => self.foreground = Some(color),
            DefaultColorKind::Background => self.background = Some(color),
        }
        self
    }

    pub fn is_empty(self) -> bool {
        self.foreground.is_none() && self.background.is_none()
    }
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

/// Parse an OSC 4 palette-color response (`ESC ] 4 ; <index> ; rgb:.. ST`).
/// Only indices within the ANSI range (0..16) are reported.
pub fn parse_palette_color_response(sequence: &str) -> Option<(u8, RgbColor)> {
    let body = sequence.strip_prefix("\x1b]")?;
    let body = body
        .strip_suffix("\x1b\\")
        .or_else(|| body.strip_suffix('\u{7}'))?;
    let rest = body.strip_prefix("4;")?;
    let (index, value) = rest.split_once(';')?;
    let index: u8 = index.parse().ok()?;
    if usize::from(index) >= HOST_ANSI_PALETTE_LEN {
        return None;
    }
    Some((index, parse_rgb_color(value)?))
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
    fn parses_palette_color_response() {
        let parsed = parse_palette_color_response("\x1b]4;4;rgb:8989/b4b4/fafa\x1b\\");
        assert_eq!(
            parsed,
            Some((
                4,
                RgbColor {
                    r: 0x89,
                    g: 0xb4,
                    b: 0xfa,
                },
            ))
        );
        // Out-of-range indices (256-color cube/grayscale) are ignored.
        assert_eq!(
            parse_palette_color_response("\x1b]4;200;rgb:1111/2222/3333\x1b\\"),
            None
        );
        // OSC 10/11 default-color responses are not palette responses.
        assert_eq!(
            parse_palette_color_response("\x1b]11;rgb:1111/2222/3333\x1b\\"),
            None
        );
    }

    #[test]
    fn palette_query_sequence_covers_sixteen_ansi_colors() {
        let seq = host_palette_query_sequence();
        assert!(seq.starts_with("\x1b]4;0;?\x1b\\"));
        assert!(seq.contains("\x1b]4;15;?\x1b\\"));
        assert_eq!(seq.matches("\x1b]4;").count(), HOST_ANSI_PALETTE_LEN);
    }

    #[test]
    fn scales_short_hex_components() {
        assert_eq!(parse_hex_component("f"), Some(255));
        assert_eq!(parse_hex_component("80"), Some(128));
        assert_eq!(parse_hex_component("800"), Some(128));
        assert_eq!(parse_hex_component("8000"), Some(128));
    }
}
