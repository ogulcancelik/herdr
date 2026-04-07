use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, ModifierKeyCode};

use super::{
    encode_cursor_key, encode_key, encode_mouse_button, encode_mouse_scroll, encode_terminal_key,
    parse_terminal_key_sequence, KeyboardProtocol, MouseProtocolEncoding, TerminalKey,
};

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
fn legacy_enter() {
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), vec![b'\r']);
}

#[test]
fn legacy_ctrl_c() {
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), vec![3]);
}

#[test]
fn legacy_shift_enter_is_just_cr() {
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    // Enter/Tab/Backspace/Esc aren't special keys with xterm modifier encoding,
    // so Shift+Enter falls through to legacy which just sends CR
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), vec![b'\r']);
}

#[test]
fn legacy_alt_up() {
    let key = KeyEvent::new(KeyCode::Up, KeyModifiers::ALT);
    // xterm modified key format: CSI 1;3A (3 = 1 + Alt)
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[1;3A");
}

#[test]
fn legacy_shift_right() {
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[1;2C");
}

#[test]
fn legacy_ctrl_left() {
    let key = KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[1;5D");
}

#[test]
fn legacy_ctrl_shift_end() {
    let key = KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[1;6F");
}

#[test]
fn legacy_alt_delete() {
    let key = KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[3;3~");
}

#[test]
fn legacy_shift_f5() {
    let key = KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1b[15;2~");
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
fn legacy_alt_char_still_esc_prefix() {
    // Alt+a on character keys still uses ESC prefix (not xterm modified)
    let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"\x1ba");
}

#[test]
fn application_cursor_keys_use_ss3_sequences() {
    assert_eq!(encode_cursor_key(KeyCode::Up, true), b"\x1bOA");
    assert_eq!(encode_cursor_key(KeyCode::Down, true), b"\x1bOB");
}

#[test]
fn normal_cursor_keys_use_csi_sequences() {
    assert_eq!(encode_cursor_key(KeyCode::Up, false), b"\x1b[A");
    assert_eq!(encode_cursor_key(KeyCode::Down, false), b"\x1b[B");
}

#[test]
fn sgr_mouse_scroll_encodes_wheel_button_and_coordinates() {
    let encoded = encode_mouse_scroll(
        crossterm::event::MouseEventKind::ScrollDown,
        4,
        6,
        KeyModifiers::SHIFT,
        MouseProtocolEncoding::Sgr,
    )
    .expect("mouse scroll should encode");

    assert_eq!(encoded, b"\x1b[<69;5;7M");
}

#[test]
fn sgr_mouse_release_keeps_button_code() {
    let encoded = encode_mouse_button(
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        11,
        9,
        KeyModifiers::empty(),
        MouseProtocolEncoding::Sgr,
    )
    .expect("mouse release should encode");

    assert_eq!(encoded, b"\x1b[<0;12;10m");
}

#[test]
fn kitty_shift_enter() {
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[13;2u"
    );
}

#[test]
fn kitty_ctrl_shift_a() {
    let key = KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[97;6u"
    );
}

#[test]
fn kitty_shift_uppercase_letter_sends_text() {
    let key = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Kitty { flags: 1 }), b"L");
}

#[test]
fn kitty_shift_uppercase_letter_ignores_alternate_key_reporting_for_text() {
    let key = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Kitty { flags: 7 }), b"L");
}

#[test]
fn kitty_shift_lowercase_letter_sends_uppercase_text() {
    let key = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Kitty { flags: 1 }), b"L");
}

#[test]
fn kitty_alt_shift_uppercase_letter_uses_base_codepoint() {
    let key = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[108;4u"
    );
}

#[test]
fn kitty_ctrl_shift_uppercase_letter_uses_base_codepoint() {
    let key = KeyEvent::new(
        KeyCode::Char('L'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[108;6u"
    );
}

#[test]
fn legacy_shift_uppercase_letter_stays_uppercase() {
    let key = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT);
    assert_eq!(encode_key(key, KeyboardProtocol::Legacy), b"L");
}

#[test]
fn kitty_alt_enter() {
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[13;3u"
    );
}

#[test]
fn kitty_plain_ctrl_c_uses_legacy() {
    // Plain Ctrl+letter is well-represented in legacy
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        vec![3]
    );
}

#[test]
fn kitty_unmodified_uses_legacy() {
    let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
    assert_eq!(encode_key(key, KeyboardProtocol::Kitty { flags: 1 }), b"a");
}

#[test]
fn kitty_shift_tab() {
    let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[9;2u"
    );
}

#[test]
fn kitty_ctrl_shift_enter() {
    let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 1 }),
        b"\x1b[13;6u"
    );
}

#[test]
fn kitty_repeat_event_type_is_encoded_when_requested() {
    let key = KeyEvent::new_with_kind(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
        crossterm::event::KeyEventKind::Repeat,
    );
    assert_eq!(
        encode_key(key, KeyboardProtocol::Kitty { flags: 3 }),
        b"\x1b[13;2:2u"
    );
}

#[test]
fn kitty_shift_letter_release_does_not_emit_text() {
    let key = KeyEvent::new_with_kind(
        KeyCode::Char('L'),
        KeyModifiers::SHIFT,
        crossterm::event::KeyEventKind::Release,
    );
    assert_eq!(encode_key(key, KeyboardProtocol::Kitty { flags: 7 }), b"");
}

#[test]
fn kitty_shifted_symbol_sends_text() {
    let key = TerminalKey::new(KeyCode::Char('1'), KeyModifiers::SHIFT)
        .with_shifted_codepoint('!' as u32);
    assert_eq!(
        encode_terminal_key(key, KeyboardProtocol::Kitty { flags: 7 }),
        b"!"
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

    // Ctrl+[ is byte-identical to Escape in legacy terminals, so the parser
    // intentionally treats 0x1b as Escape and only disambiguates the other
    // legacy control-symbol bytes here.
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
fn legacy_modified_special_roundtrip_matrix() {
    let cases = [
        KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Down, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::ALT),
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Insert, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT),
    ];

    for key in cases {
        let encoded = encode_key(key, KeyboardProtocol::Legacy);
        let parsed = parse_terminal_key_sequence(std::str::from_utf8(&encoded).unwrap()).unwrap();
        assert_terminal_key_eq(parsed, key.code, key.modifiers, key.kind, None);
    }
}

#[test]
fn kitty_shifted_symbol_prefers_text_over_roundtrip_key_identity() {
    let key = TerminalKey::new(KeyCode::Char('1'), KeyModifiers::SHIFT)
        .with_shifted_codepoint('!' as u32);
    let encoded = encode_terminal_key(key, KeyboardProtocol::Kitty { flags: 7 });
    assert_eq!(encoded, b"!");
}

#[test]
fn legacy_basic_special_roundtrip_matrix() {
    let cases = [
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Left, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Home, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::End, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Insert, KeyModifiers::empty()),
        KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()),
    ];

    for key in cases {
        let encoded = encode_key(key, KeyboardProtocol::Legacy);
        let parsed = parse_terminal_key_sequence(std::str::from_utf8(&encoded).unwrap()).unwrap();
        assert_terminal_key_eq(parsed, key.code, key.modifiers, key.kind, None);
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

#[test]
fn kitty_shifted_symbol_pair_matrix_is_encoded_as_text() {
    let cases = [('1', '!'), ('/', '?'), ('[', '{')];

    for (base, shifted) in cases {
        let key = TerminalKey::new(KeyCode::Char(base), KeyModifiers::SHIFT)
            .with_shifted_codepoint(shifted as u32);
        let encoded = encode_terminal_key(key, KeyboardProtocol::Kitty { flags: 7 });
        assert_eq!(encoded, shifted.to_string().into_bytes(), "base={base}");
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

#[test]
fn protocol_from_zero_flags_is_legacy() {
    assert_eq!(
        KeyboardProtocol::from_kitty_flags(0),
        KeyboardProtocol::Legacy
    );
}

#[test]
fn protocol_from_nonzero_flags_is_kitty() {
    assert_eq!(
        KeyboardProtocol::from_kitty_flags(7),
        KeyboardProtocol::Kitty { flags: 7 }
    );
}

#[test]
fn chinese_char_encodes_as_utf8() {
    let key = TerminalKey::new(KeyCode::Char('中'), KeyModifiers::empty());
    let encoded = encode_terminal_key(key, KeyboardProtocol::Legacy);
    assert_eq!(encoded, "中".as_bytes());
}

#[test]
fn chinese_char_with_kitty_protocol_encodes_as_utf8() {
    let key = TerminalKey::new(KeyCode::Char('文'), KeyModifiers::empty());
    let encoded = encode_terminal_key(key, KeyboardProtocol::Kitty { flags: 7 });
    assert_eq!(encoded, "文".as_bytes());
}

#[test]
fn chinese_char_with_modifiers_falls_back_to_kitty_encoding() {
    let key = TerminalKey::new(KeyCode::Char('测'), KeyModifiers::ALT);
    let encoded = encode_terminal_key(key, KeyboardProtocol::Kitty { flags: 7 });
    assert!(!encoded.is_empty());
    assert_ne!(encoded, "测".as_bytes());
}
