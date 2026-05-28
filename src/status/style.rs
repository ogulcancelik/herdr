//! tmux `#[...]` style-directive parsing into ratatui [`Style`].
//!
//! Supports the attribute set the status bar uses: `fg=`, `bg=`, `bold`/`nobold`,
//! `dim`, `italics`, `underscore`/`underline`, `reverse`, `blink`, `hidden`,
//! `strikethrough`, and `default`/`none` (reset to the base style). Unknown tokens
//! (e.g. `align=...`) are ignored rather than erroring, matching tmux's leniency.

use ratatui::style::{Color, Modifier, Style};

/// Parse a tmux color token into a ratatui [`Color`].
///
/// Extends [`crate::config::theme::parse_color`] with tmux's `colourN`/`colorN`
/// and bare-integer 256-color indices. Returns `None` only for an empty token.
pub fn parse_status_color(token: &str) -> Option<Color> {
    let s = token.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("default") || s.eq_ignore_ascii_case("none") {
        return Some(Color::Reset);
    }
    let lower = s.to_ascii_lowercase();
    let idx = lower
        .strip_prefix("colour")
        .or_else(|| lower.strip_prefix("color"));
    if let Some(n) = idx {
        if let Ok(i) = n.parse::<u8>() {
            return Some(Color::Indexed(i));
        }
    }
    if let Ok(i) = s.parse::<u8>() {
        return Some(Color::Indexed(i));
    }
    // hex (#rrggbb / #rgb), named, rgb(r,g,b), reset aliases.
    Some(crate::config::parse_color(s))
}

/// Apply a (comma-separated) style spec on top of `current`, with `base` used as
/// the reset target for `default`/`none`. The spec must already have had its
/// `#{...}` expansions resolved.
pub fn apply_style_spec(spec: &str, base: Style, current: Style) -> Style {
    let mut style = current;
    for raw in spec.split(',') {
        let tok = raw.trim();
        if tok.is_empty() {
            continue;
        }
        let lower = tok.to_ascii_lowercase();
        match lower.as_str() {
            "default" | "none" => style = base,
            "bold" | "bright" => style = style.add_modifier(Modifier::BOLD),
            "nobold" | "norm" => style = style.remove_modifier(Modifier::BOLD),
            "dim" => style = style.add_modifier(Modifier::DIM),
            "italics" | "italic" => style = style.add_modifier(Modifier::ITALIC),
            "underscore" | "underline" => style = style.add_modifier(Modifier::UNDERLINED),
            "reverse" => style = style.add_modifier(Modifier::REVERSED),
            "blink" => style = style.add_modifier(Modifier::SLOW_BLINK),
            "hidden" => style = style.add_modifier(Modifier::HIDDEN),
            "strikethrough" => style = style.add_modifier(Modifier::CROSSED_OUT),
            _ => {
                if let Some(v) = lower.strip_prefix("fg=") {
                    if let Some(c) = parse_status_color(v) {
                        style = style.fg(c);
                    }
                } else if let Some(v) = lower.strip_prefix("bg=") {
                    if let Some(c) = parse_status_color(v) {
                        style = style.bg(c);
                    }
                }
                // Unknown tokens (align=, list=, range=, push/pop) are ignored.
            }
        }
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_index_parses() {
        assert_eq!(parse_status_color("colour167"), Some(Color::Indexed(167)));
        assert_eq!(parse_status_color("color16"), Some(Color::Indexed(16)));
        assert_eq!(parse_status_color("244"), Some(Color::Indexed(244)));
    }

    #[test]
    fn default_resets() {
        assert_eq!(parse_status_color("default"), Some(Color::Reset));
    }

    #[test]
    fn style_spec_applies_fg_bg_bold() {
        let s = apply_style_spec("fg=colour16,bg=colour167,bold", Style::default(), Style::default());
        assert_eq!(s.fg, Some(Color::Indexed(16)));
        assert_eq!(s.bg, Some(Color::Indexed(167)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn default_token_resets_to_base() {
        let base = Style::default().fg(Color::Red);
        let cur = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
        let s = apply_style_spec("default", base, cur);
        assert_eq!(s.fg, Some(Color::Red));
        assert!(!s.add_modifier.contains(Modifier::BOLD));
    }
}
