use crossterm::event::{KeyCode, KeyModifiers, MediaKeyCode, ModifierKeyCode};

use super::TerminalKey;

#[allow(dead_code)] // Next step: raw stdin parser will feed TerminalKey directly through this path.
pub fn parse_terminal_key_sequence(data: &str) -> Option<TerminalKey> {
    parse_kitty_key_sequence(data)
        .or_else(|| parse_modify_other_keys_sequence(data))
        .or_else(|| parse_legacy_key_sequence(data))
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn parse_kitty_key_sequence(data: &str) -> Option<TerminalKey> {
    let body = data.strip_prefix("\x1b[")?.strip_suffix('u')?;

    let (main, event_type) = match body.rsplit_once(':') {
        Some((head, tail)) if tail.chars().all(|ch| ch.is_ascii_digit()) && head.contains(';') => {
            (head, Some(tail))
        }
        _ => (body, None),
    };

    let (key_part, modifier_part) = main.rsplit_once(';').unwrap_or((main, "1"));
    let modifier = modifier_part.parse::<u8>().ok()?.checked_sub(1)?;

    let mut key_fields = key_part.split(':');
    let codepoint = key_fields.next()?.parse::<u32>().ok()?;
    let shifted_codepoint = key_fields
        .next()
        .filter(|field| !field.is_empty())
        .and_then(|field| field.parse::<u32>().ok());

    let code = kitty_codepoint_to_keycode(codepoint)?;
    let kind = parse_kitty_event_type(event_type)?;

    Some(TerminalKey {
        code,
        modifiers: key_modifiers_from_u8(modifier),
        kind,
        shifted_codepoint,
    })
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn parse_modify_other_keys_sequence(data: &str) -> Option<TerminalKey> {
    let body = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let (modifier_part, codepoint_part) = body.split_once(';')?;
    let modifier = modifier_part.parse::<u8>().ok()?.checked_sub(1)?;
    let codepoint = codepoint_part.parse::<u32>().ok()?;

    Some(TerminalKey::new(
        kitty_codepoint_to_keycode(codepoint)?,
        key_modifiers_from_u8(modifier),
    ))
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn parse_legacy_key_sequence(data: &str) -> Option<TerminalKey> {
    if let Some(key) = parse_legacy_special_sequence(data) {
        return Some(key);
    }

    match data {
        // In raw mode, Enter is carriage return. A bare line feed is ambiguous in
        // terminal input and is commonly used for Ctrl+J or Shift+Enter workarounds.
        // Preserve LF by letting it fall through to legacy control-byte parsing.
        "\r" => Some(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty())),
        "\t" => Some(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty())),
        "\x1b" => Some(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty())),
        "\x7f" => Some(TerminalKey::new(KeyCode::Backspace, KeyModifiers::empty())),
        _ if data.starts_with('\x1b') => {
            let rest = data.strip_prefix('\x1b')?;
            if rest.chars().count() == 1 {
                let ch = rest.chars().next()?;
                Some(TerminalKey::new(KeyCode::Char(ch), KeyModifiers::ALT))
            } else {
                None
            }
        }
        _ if data.chars().count() == 1 => {
            let ch = data.chars().next()?;

            if let Some(ctrl_key) = parse_legacy_ctrl_char(ch) {
                return Some(ctrl_key);
            }

            let mut modifiers = KeyModifiers::empty();
            let code = if ch.is_ascii_uppercase() {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Char(ch)
            } else {
                KeyCode::Char(ch)
            };
            Some(TerminalKey::new(code, modifiers))
        }
        _ => None,
    }
}

fn parse_legacy_ctrl_char(ch: char) -> Option<TerminalKey> {
    match ch as u32 {
        0 => Some(TerminalKey::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
        1..=26 => Some(TerminalKey::new(
            KeyCode::Char(char::from_u32((ch as u32) + 96)?),
            KeyModifiers::CONTROL,
        )),
        27 => Some(TerminalKey::new(KeyCode::Char('['), KeyModifiers::CONTROL)),
        28 => Some(TerminalKey::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
        29 => Some(TerminalKey::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
        30 => Some(TerminalKey::new(KeyCode::Char('^'), KeyModifiers::CONTROL)),
        31 => Some(TerminalKey::new(KeyCode::Char('-'), KeyModifiers::CONTROL)),
        _ => None,
    }
}

fn parse_legacy_special_sequence(data: &str) -> Option<TerminalKey> {
    match data {
        "\x1b\x1b[A" => Some(TerminalKey::new(KeyCode::Up, KeyModifiers::ALT)),
        "\x1b\x1b[B" => Some(TerminalKey::new(KeyCode::Down, KeyModifiers::ALT)),
        "\x1b\x1b[C" => Some(TerminalKey::new(KeyCode::Right, KeyModifiers::ALT)),
        "\x1b\x1b[D" => Some(TerminalKey::new(KeyCode::Left, KeyModifiers::ALT)),
        "\x1b[A" | "\x1bOA" => Some(TerminalKey::new(KeyCode::Up, KeyModifiers::empty())),
        "\x1b[B" | "\x1bOB" => Some(TerminalKey::new(KeyCode::Down, KeyModifiers::empty())),
        "\x1b[C" | "\x1bOC" => Some(TerminalKey::new(KeyCode::Right, KeyModifiers::empty())),
        "\x1b[D" | "\x1bOD" => Some(TerminalKey::new(KeyCode::Left, KeyModifiers::empty())),
        "\x1b[H" | "\x1bOH" | "\x1b[1~" | "\x1b[7~" => {
            Some(TerminalKey::new(KeyCode::Home, KeyModifiers::empty()))
        }
        "\x1b[F" | "\x1bOF" | "\x1b[4~" | "\x1b[8~" => {
            Some(TerminalKey::new(KeyCode::End, KeyModifiers::empty()))
        }
        "\x1b[5~" => Some(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty())),
        "\x1b[6~" => Some(TerminalKey::new(KeyCode::PageDown, KeyModifiers::empty())),
        "\x1b[2~" => Some(TerminalKey::new(KeyCode::Insert, KeyModifiers::empty())),
        "\x1b[3~" => Some(TerminalKey::new(KeyCode::Delete, KeyModifiers::empty())),
        "\x1bOP" => Some(TerminalKey::new(KeyCode::F(1), KeyModifiers::empty())),
        "\x1bOQ" => Some(TerminalKey::new(KeyCode::F(2), KeyModifiers::empty())),
        "\x1bOR" => Some(TerminalKey::new(KeyCode::F(3), KeyModifiers::empty())),
        "\x1bOS" => Some(TerminalKey::new(KeyCode::F(4), KeyModifiers::empty())),
        "\x1b[15~" => Some(TerminalKey::new(KeyCode::F(5), KeyModifiers::empty())),
        "\x1b[17~" => Some(TerminalKey::new(KeyCode::F(6), KeyModifiers::empty())),
        "\x1b[18~" => Some(TerminalKey::new(KeyCode::F(7), KeyModifiers::empty())),
        "\x1b[19~" => Some(TerminalKey::new(KeyCode::F(8), KeyModifiers::empty())),
        "\x1b[20~" => Some(TerminalKey::new(KeyCode::F(9), KeyModifiers::empty())),
        "\x1b[21~" => Some(TerminalKey::new(KeyCode::F(10), KeyModifiers::empty())),
        "\x1b[23~" => Some(TerminalKey::new(KeyCode::F(11), KeyModifiers::empty())),
        "\x1b[24~" => Some(TerminalKey::new(KeyCode::F(12), KeyModifiers::empty())),
        "\x1b[Z" => Some(TerminalKey::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        _ => parse_xterm_modified_special_sequence(data),
    }
}

fn parse_xterm_modified_special_sequence(data: &str) -> Option<TerminalKey> {
    let body = data.strip_prefix("\x1b[")?;

    if let Some(body) = body.strip_prefix("1;") {
        let suffix_char = body.chars().last()?;
        if suffix_char.is_ascii_alphabetic() {
            let modifier_and_event = body.strip_suffix(suffix_char)?;
            let (modifier_text, event_type) = split_modifier_and_event(modifier_and_event);
            let mod_value = modifier_text.parse::<u8>().ok()?.checked_sub(1)?;
            let code = match suffix_char {
                'A' => KeyCode::Up,
                'B' => KeyCode::Down,
                'C' => KeyCode::Right,
                'D' => KeyCode::Left,
                'H' => KeyCode::Home,
                'F' => KeyCode::End,
                'P' => KeyCode::F(1),
                'Q' => KeyCode::F(2),
                'R' => KeyCode::F(3),
                'S' => KeyCode::F(4),
                _ => return None,
            };
            return Some(
                TerminalKey::new(code, key_modifiers_from_u8(mod_value))
                    .with_kind(parse_kitty_event_type(event_type)?),
            );
        }
    }

    let tilde_body = body.strip_suffix('~')?;
    let (code_part, modifier_part) = tilde_body.split_once(';')?;
    let mod_value = modifier_part.parse::<u8>().ok()?.checked_sub(1)?;
    let code = match code_part {
        "2" => KeyCode::Insert,
        "3" => KeyCode::Delete,
        "5" => KeyCode::PageUp,
        "6" => KeyCode::PageDown,
        "15" => KeyCode::F(5),
        "17" => KeyCode::F(6),
        "18" => KeyCode::F(7),
        "19" => KeyCode::F(8),
        "20" => KeyCode::F(9),
        "21" => KeyCode::F(10),
        "23" => KeyCode::F(11),
        "24" => KeyCode::F(12),
        _ => return None,
    };
    Some(TerminalKey::new(code, key_modifiers_from_u8(mod_value)))
}

fn split_modifier_and_event(input: &str) -> (&str, Option<&str>) {
    match input.split_once(':') {
        Some((modifier, event)) if !modifier.is_empty() => (modifier, Some(event)),
        _ => (input, None),
    }
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn parse_kitty_event_type(value: Option<&str>) -> Option<crossterm::event::KeyEventKind> {
    match value.unwrap_or("1") {
        "1" => Some(crossterm::event::KeyEventKind::Press),
        "2" => Some(crossterm::event::KeyEventKind::Repeat),
        "3" => Some(crossterm::event::KeyEventKind::Release),
        _ => None,
    }
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn kitty_codepoint_to_keycode(codepoint: u32) -> Option<KeyCode> {
    match codepoint {
        8 | 127 => Some(KeyCode::Backspace),
        9 => Some(KeyCode::Tab),
        13 | 57414 => Some(KeyCode::Enter),
        27 => Some(KeyCode::Esc),
        57358 => Some(KeyCode::CapsLock),
        57359 => Some(KeyCode::ScrollLock),
        57360 => Some(KeyCode::NumLock),
        57361 => Some(KeyCode::PrintScreen),
        57362 => Some(KeyCode::Pause),
        57363 => Some(KeyCode::Menu),
        57376..=57398 => Some(KeyCode::F((codepoint - 57376 + 13) as u8)),
        57417 => Some(KeyCode::Left),
        57418 => Some(KeyCode::Right),
        57419 => Some(KeyCode::Up),
        57420 => Some(KeyCode::Down),
        57421 => Some(KeyCode::PageUp),
        57422 => Some(KeyCode::PageDown),
        57423 => Some(KeyCode::Home),
        57424 => Some(KeyCode::End),
        57425 => Some(KeyCode::Insert),
        57426 => Some(KeyCode::Delete),
        57427 => Some(KeyCode::KeypadBegin),
        57428 => Some(KeyCode::Media(MediaKeyCode::Play)),
        57429 => Some(KeyCode::Media(MediaKeyCode::Pause)),
        57430 => Some(KeyCode::Media(MediaKeyCode::PlayPause)),
        57431 => Some(KeyCode::Media(MediaKeyCode::Reverse)),
        57432 => Some(KeyCode::Media(MediaKeyCode::Stop)),
        57433 => Some(KeyCode::Media(MediaKeyCode::FastForward)),
        57434 => Some(KeyCode::Media(MediaKeyCode::Rewind)),
        57435 => Some(KeyCode::Media(MediaKeyCode::TrackNext)),
        57436 => Some(KeyCode::Media(MediaKeyCode::TrackPrevious)),
        57437 => Some(KeyCode::Media(MediaKeyCode::Record)),
        57438 => Some(KeyCode::Media(MediaKeyCode::LowerVolume)),
        57439 => Some(KeyCode::Media(MediaKeyCode::RaiseVolume)),
        57440 => Some(KeyCode::Media(MediaKeyCode::MuteVolume)),
        57441 => Some(KeyCode::Modifier(ModifierKeyCode::LeftShift)),
        57442 => Some(KeyCode::Modifier(ModifierKeyCode::LeftControl)),
        57443 => Some(KeyCode::Modifier(ModifierKeyCode::LeftAlt)),
        57444 => Some(KeyCode::Modifier(ModifierKeyCode::LeftSuper)),
        57445 => Some(KeyCode::Modifier(ModifierKeyCode::LeftHyper)),
        57446 => Some(KeyCode::Modifier(ModifierKeyCode::LeftMeta)),
        57447 => Some(KeyCode::Modifier(ModifierKeyCode::RightShift)),
        57448 => Some(KeyCode::Modifier(ModifierKeyCode::RightControl)),
        57449 => Some(KeyCode::Modifier(ModifierKeyCode::RightAlt)),
        57450 => Some(KeyCode::Modifier(ModifierKeyCode::RightSuper)),
        57451 => Some(KeyCode::Modifier(ModifierKeyCode::RightHyper)),
        57452 => Some(KeyCode::Modifier(ModifierKeyCode::RightMeta)),
        57453 => Some(KeyCode::Modifier(ModifierKeyCode::IsoLevel3Shift)),
        57454 => Some(KeyCode::Modifier(ModifierKeyCode::IsoLevel5Shift)),
        value if is_kitty_functional_codepoint(value) => None,
        value => char::from_u32(value).map(KeyCode::Char),
    }
}

fn is_kitty_functional_codepoint(codepoint: u32) -> bool {
    (57358..=57454).contains(&codepoint)
}

#[allow(dead_code)] // Reserved for the upcoming raw stdin parser.
fn key_modifiers_from_u8(modifier: u8) -> KeyModifiers {
    let mut mods = KeyModifiers::empty();
    if modifier & 0b0000_0001 != 0 {
        mods |= KeyModifiers::SHIFT;
    }
    if modifier & 0b0000_0010 != 0 {
        mods |= KeyModifiers::ALT;
    }
    if modifier & 0b0000_0100 != 0 {
        mods |= KeyModifiers::CONTROL;
    }
    if modifier & 0b0000_1000 != 0 {
        mods |= KeyModifiers::SUPER;
    }
    if modifier & 0b0001_0000 != 0 {
        mods |= KeyModifiers::HYPER;
    }
    if modifier & 0b0010_0000 != 0 {
        mods |= KeyModifiers::META;
    }
    mods
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers, ModifierKeyCode};

    use super::*;
    use crate::input::{encode_terminal_key, KeyboardProtocol};

    fn assert_terminal_key_eq(
        actual: TerminalKey,
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: crossterm::event::KeyEventKind,
        shifted_codepoint: Option<u32>,
    ) {
        assert_eq!(actual.code, code);
        assert_eq!(actual.modifiers, modifiers);
        assert_eq!(actual.kind, kind);
        assert_eq!(actual.shifted_codepoint, shifted_codepoint);
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        let hex = hex.trim();
        assert_eq!(hex.len() % 2, 0, "hex string must have even length");
        (0..hex.len())
            .step_by(2)
            .map(|idx| u8::from_str_radix(&hex[idx..idx + 2], 16).unwrap())
            .collect()
    }

    fn parse_fixture_key_code(value: &str) -> KeyCode {
        match value {
            "enter" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "esc" => KeyCode::Esc,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "insert" => KeyCode::Insert,
            "delete" => KeyCode::Delete,
            value if value.starts_with("char:") => {
                KeyCode::Char(value.trim_start_matches("char:").chars().next().unwrap())
            }
            other => panic!("unsupported fixture key code: {other}"),
        }
    }

    fn parse_fixture_modifiers(value: &str) -> KeyModifiers {
        if value == "-" || value.is_empty() {
            return KeyModifiers::empty();
        }

        let mut modifiers = KeyModifiers::empty();
        for part in value.split('+') {
            match part {
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "alt" => modifiers |= KeyModifiers::ALT,
                "control" => modifiers |= KeyModifiers::CONTROL,
                "super" => modifiers |= KeyModifiers::SUPER,
                "hyper" => modifiers |= KeyModifiers::HYPER,
                "meta" => modifiers |= KeyModifiers::META,
                other => panic!("unsupported fixture modifier: {other}"),
            }
        }
        modifiers
    }

    fn parse_fixture_kind(value: &str) -> crossterm::event::KeyEventKind {
        match value {
            "press" => crossterm::event::KeyEventKind::Press,
            "repeat" => crossterm::event::KeyEventKind::Repeat,
            "release" => crossterm::event::KeyEventKind::Release,
            other => panic!("unsupported fixture kind: {other}"),
        }
    }

    #[test]
    fn parse_legacy_f_keys() {
        assert_terminal_key_eq(
            parse_terminal_key_sequence("\x1bOP").expect("f1 should parse"),
            KeyCode::F(1),
            KeyModifiers::empty(),
            crossterm::event::KeyEventKind::Press,
            None,
        );
        assert_terminal_key_eq(
            parse_terminal_key_sequence("\x1b[15~").expect("f5 should parse"),
            KeyCode::F(5),
            KeyModifiers::empty(),
            crossterm::event::KeyEventKind::Press,
            None,
        );
    }

    #[test]
    fn parse_modified_f_keys() {
        assert_terminal_key_eq(
            parse_terminal_key_sequence("\x1b[1;2P").expect("shift+f1 should parse"),
            KeyCode::F(1),
            KeyModifiers::SHIFT,
            crossterm::event::KeyEventKind::Press,
            None,
        );
        assert_terminal_key_eq(
            parse_terminal_key_sequence("\x1b[15;2~").expect("shift+f5 should parse"),
            KeyCode::F(5),
            KeyModifiers::SHIFT,
            crossterm::event::KeyEventKind::Press,
            None,
        );
    }

    #[test]
    fn parse_kitty_sequence_preserves_shifted_symbol_pair() {
        let key = parse_terminal_key_sequence("\x1b[49:33;2:1u").unwrap();
        assert_eq!(key.code, KeyCode::Char('1'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Press);
        assert_eq!(key.shifted_codepoint, Some('!' as u32));
    }

    #[test]
    fn parse_kitty_sequence_preserves_shifted_letter_pair_and_release() {
        let key = parse_terminal_key_sequence("\x1b[108:76;2:3u").unwrap();
        assert_eq!(key.code, KeyCode::Char('l'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Release);
        assert_eq!(key.shifted_codepoint, Some('L' as u32));
    }

    #[test]
    fn parse_modify_other_keys_sequence() {
        let key = parse_terminal_key_sequence("\x1b[27;6;108~").unwrap();
        assert_eq!(key.code, KeyCode::Char('l'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Press);
        assert_eq!(key.shifted_codepoint, None);
    }

    #[test]
    fn parse_legacy_uppercase_letter_as_shifted_char() {
        let key = parse_terminal_key_sequence("L").unwrap();
        assert_eq!(key.code, KeyCode::Char('L'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_legacy_up_arrow_sequence() {
        let key = parse_terminal_key_sequence("\x1b[A").unwrap();
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parse_kitty_modifier_sequence() {
        let key = parse_terminal_key_sequence("\x1b[57441;2:1u").unwrap();
        assert_eq!(key.code, KeyCode::Modifier(ModifierKeyCode::LeftShift));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Press);
    }

    #[test]
    fn parse_ghostty_enhanced_up_arrow_press_sequence() {
        let key = parse_terminal_key_sequence("\x1b[1;1:1A").unwrap();
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Press);
    }

    #[test]
    fn parse_ghostty_enhanced_up_arrow_release_sequence() {
        let key = parse_terminal_key_sequence("\x1b[1;1:3A").unwrap();
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, crossterm::event::KeyEventKind::Release);
    }

    #[test]
    fn parse_xterm_alt_up_arrow_sequence() {
        let key = parse_terminal_key_sequence("\x1b[1;3A").unwrap();
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parse_xterm_alt_down_arrow_sequence() {
        let key = parse_terminal_key_sequence("\x1b[1;3B").unwrap();
        assert_eq!(key.code, KeyCode::Down);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parse_kitty_functional_up_arrow_sequence() {
        let key = parse_terminal_key_sequence("\x1b[57419;1u").unwrap();
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parse_legacy_ctrl_b_sequence() {
        let key = parse_terminal_key_sequence("\x02").unwrap();
        assert_eq!(key.code, KeyCode::Char('b'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_legacy_ctrl_c_sequence() {
        let key = parse_terminal_key_sequence("\x03").unwrap();
        assert_eq!(key.code, KeyCode::Char('c'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_legacy_lf_sequence_as_ctrl_j() {
        let key = parse_terminal_key_sequence("\n").unwrap();
        assert_eq!(key.code, KeyCode::Char('j'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn legacy_lf_roundtrips_as_lf() {
        let key = parse_terminal_key_sequence("\n").unwrap();
        assert_eq!(encode_terminal_key(key, KeyboardProtocol::Legacy), b"\n");
    }

    #[test]
    fn legacy_ctrl_byte_matrix_is_covered() {
        for (byte, expected) in [
            (b'\x01', 'a'),
            (b'\x02', 'b'),
            (b'\x03', 'c'),
            (b'\x1a', 'z'),
        ] {
            let key = parse_terminal_key_sequence(std::str::from_utf8(&[byte]).unwrap()).unwrap();
            assert_terminal_key_eq(
                key,
                KeyCode::Char(expected),
                KeyModifiers::CONTROL,
                crossterm::event::KeyEventKind::Press,
                None,
            );
        }

        for (byte, expected) in [
            (b'\x1c', '\\'),
            (b'\x1d', ']'),
            (b'\x1e', '^'),
            (b'\x1f', '-'),
        ] {
            let key = parse_terminal_key_sequence(std::str::from_utf8(&[byte]).unwrap()).unwrap();
            assert_terminal_key_eq(
                key,
                KeyCode::Char(expected),
                KeyModifiers::CONTROL,
                crossterm::event::KeyEventKind::Press,
                None,
            );
        }
    }

    #[test]
    fn kitty_functional_key_matrix_is_covered() {
        let cases = [
            ("\x1b[57417;1u", KeyCode::Left),
            ("\x1b[57418;1u", KeyCode::Right),
            ("\x1b[57419;1u", KeyCode::Up),
            ("\x1b[57420;1u", KeyCode::Down),
            ("\x1b[57421;1u", KeyCode::PageUp),
            ("\x1b[57422;1u", KeyCode::PageDown),
            ("\x1b[57423;1u", KeyCode::Home),
            ("\x1b[57424;1u", KeyCode::End),
            ("\x1b[57425;1u", KeyCode::Insert),
            ("\x1b[57426;1u", KeyCode::Delete),
        ];

        for (sequence, code) in cases {
            let parsed = parse_terminal_key_sequence(sequence).unwrap();
            assert_terminal_key_eq(
                parsed,
                code,
                KeyModifiers::empty(),
                crossterm::event::KeyEventKind::Press,
                None,
            );
        }
    }

    fn assert_fixture_corpus_parses(corpus: &str) {
        for line in corpus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut columns: Vec<_> = line.split('\t').collect();
            if columns.len() == 5 {
                columns.push("");
            }

            let (family, bytes_hex, code, modifiers, kind, shifted) = match columns.len() {
                6 => {
                    if columns[1].chars().all(|ch| ch.is_ascii_hexdigit()) {
                        (
                            columns[0], columns[1], columns[2], columns[3], columns[4], columns[5],
                        )
                    } else {
                        (
                            columns[0], columns[2], columns[3], columns[4], columns[5], "",
                        )
                    }
                }
                7 => (
                    columns[0], columns[2], columns[3], columns[4], columns[5], columns[6],
                ),
                _ => panic!("fixture row must have 6 or 7 columns: {line}"),
            };

            assert!(
                bytes_hex.chars().all(|ch| ch.is_ascii_hexdigit()),
                "non-hex fixture bytes for {family}: {bytes_hex}"
            );
            let bytes = decode_hex(bytes_hex);
            let text = std::str::from_utf8(&bytes).unwrap();
            let parsed = parse_terminal_key_sequence(text)
                .unwrap_or_else(|| panic!("fixture failed to parse: {family}"));

            assert_terminal_key_eq(
                parsed,
                parse_fixture_key_code(code),
                parse_fixture_modifiers(modifiers),
                parse_fixture_kind(kind),
                if shifted.is_empty() {
                    None
                } else {
                    Some(shifted.parse::<u32>().unwrap())
                },
            );
        }
    }

    #[test]
    fn keyboard_protocol_corpus_fixture_parses() {
        let corpus = include_str!("../../tests/fixtures/keyboard_protocol_corpus.tsv");
        assert_fixture_corpus_parses(corpus);
    }

    #[test]
    fn macos_terminal_variants_fixture_parses() {
        let corpus = include_str!("../../tests/fixtures/macos_terminal_variants.tsv");
        for line in corpus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut columns: Vec<_> = line.split('\t').collect();
            if columns.len() == 6 {
                columns.push("");
            }
            assert_eq!(
                columns.len(),
                7,
                "macOS fixture row must have 7 columns: {line}"
            );

            let source = format!("{}:{}", columns[0], columns[1]);
            let transformed = [
                source.as_str(),
                columns[2],
                columns[3],
                columns[4],
                columns[5],
                columns[6],
            ]
            .join("\t");
            assert_fixture_corpus_parses(&transformed);
        }
    }

    #[test]
    fn linux_terminal_variants_fixture_parses() {
        let corpus = include_str!("../../tests/fixtures/linux_terminal_variants.tsv");
        assert_fixture_corpus_parses(corpus);
    }
}
