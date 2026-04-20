pub(super) fn ghostty_key_event_from_terminal_key(
    key: crate::input::TerminalKey,
) -> Option<crate::ghostty::KeyEvent> {
    let mut event = crate::ghostty::KeyEvent::new().ok()?;
    event.set_action(match key.kind {
        crossterm::event::KeyEventKind::Press => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS
        }
        crossterm::event::KeyEventKind::Release => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE
        }
        crossterm::event::KeyEventKind::Repeat => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT
        }
    });
    event.set_mods(ghostty_mods_from_key_modifiers(key.modifiers));
    event.set_key(ghostty_key_from_crossterm_key_code(
        key.code,
        key.shifted_codepoint,
    )?);

    if let Some(text) = ghostty_key_text(key) {
        event.set_utf8(&text);
    } else {
        event.set_utf8("");
    }

    if let Some(codepoint) = ghostty_unshifted_codepoint(key) {
        event.set_unshifted_codepoint(codepoint);
    }

    Some(event)
}

pub(super) fn ghostty_prefers_herdr_text_encoding(key: crate::input::TerminalKey) -> bool {
    matches!(key.code, crossterm::event::KeyCode::Char(_))
}

pub(super) fn ghostty_mods_from_key_modifiers(modifiers: crossterm::event::KeyModifiers) -> u16 {
    let mut ghostty_mods = 0u16;
    if modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
        ghostty_mods |= crate::ghostty::MOD_SHIFT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
        ghostty_mods |= crate::ghostty::MOD_CTRL;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::ALT) {
        ghostty_mods |= crate::ghostty::MOD_ALT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
        ghostty_mods |= crate::ghostty::MOD_SUPER;
    }
    ghostty_mods
}

pub(super) fn ghostty_mouse_encoder_for_terminal(
    terminal: &crate::ghostty::Terminal,
) -> Option<crate::ghostty::MouseEncoder> {
    let mut encoder = crate::ghostty::MouseEncoder::new().ok()?;
    encoder.set_from_terminal(terminal);
    let cols = terminal.cols().ok()? as u32;
    let rows = terminal.rows().ok()? as u32;
    encoder.set_size(cols, rows, 1, 1);
    Some(encoder)
}

pub(super) fn ghostty_mouse_event_from_button_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    let (action, button) = match kind {
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        _ => return None,
    };
    event.set_action(action);
    if let Some(button) = button {
        event.set_button(button);
    } else {
        event.clear_button();
    }
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

pub(super) fn ghostty_mouse_event_from_wheel_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    event.set_action(crate::ghostty::MOUSE_ACTION_PRESS);
    let button = match kind {
        crossterm::event::MouseEventKind::ScrollUp => crate::ghostty::MOUSE_BUTTON_WHEEL_UP,
        crossterm::event::MouseEventKind::ScrollDown => crate::ghostty::MOUSE_BUTTON_WHEEL_DOWN,
        crossterm::event::MouseEventKind::ScrollLeft => crate::ghostty::MOUSE_BUTTON_WHEEL_LEFT,
        crossterm::event::MouseEventKind::ScrollRight => crate::ghostty::MOUSE_BUTTON_WHEEL_RIGHT,
        _ => return None,
    };
    event.set_button(button);
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

fn ghostty_key_text(key: crate::input::TerminalKey) -> Option<String> {
    match key.code {
        crossterm::event::KeyCode::Char(c) => Some(
            key.shifted_codepoint
                .and_then(char::from_u32)
                .unwrap_or(c)
                .to_string(),
        ),
        _ => None,
    }
}

fn ghostty_unshifted_codepoint(key: crate::input::TerminalKey) -> Option<u32> {
    match key.code {
        crossterm::event::KeyCode::Char(c) => Some(c as u32),
        _ => None,
    }
}

fn ghostty_key_from_crossterm_key_code(
    code: crossterm::event::KeyCode,
    shifted_codepoint: Option<u32>,
) -> Option<u32> {
    use crate::ghostty::ffi;
    use crossterm::event::KeyCode;

    match code {
        KeyCode::Backspace => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE),
        KeyCode::Enter => Some(ffi::GhosttyKey_GHOSTTY_KEY_ENTER),
        KeyCode::Left => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT),
        KeyCode::Right => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT),
        KeyCode::Up => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP),
        KeyCode::Down => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN),
        KeyCode::Home => Some(ffi::GhosttyKey_GHOSTTY_KEY_HOME),
        KeyCode::End => Some(ffi::GhosttyKey_GHOSTTY_KEY_END),
        KeyCode::PageUp => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP),
        KeyCode::PageDown => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN),
        KeyCode::Tab | KeyCode::BackTab => Some(ffi::GhosttyKey_GHOSTTY_KEY_TAB),
        KeyCode::Delete => Some(ffi::GhosttyKey_GHOSTTY_KEY_DELETE),
        KeyCode::Insert => Some(ffi::GhosttyKey_GHOSTTY_KEY_INSERT),
        KeyCode::Esc => Some(ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE),
        KeyCode::F(n) => Some(match n {
            1 => ffi::GhosttyKey_GHOSTTY_KEY_F1,
            2 => ffi::GhosttyKey_GHOSTTY_KEY_F2,
            3 => ffi::GhosttyKey_GHOSTTY_KEY_F3,
            4 => ffi::GhosttyKey_GHOSTTY_KEY_F4,
            5 => ffi::GhosttyKey_GHOSTTY_KEY_F5,
            6 => ffi::GhosttyKey_GHOSTTY_KEY_F6,
            7 => ffi::GhosttyKey_GHOSTTY_KEY_F7,
            8 => ffi::GhosttyKey_GHOSTTY_KEY_F8,
            9 => ffi::GhosttyKey_GHOSTTY_KEY_F9,
            10 => ffi::GhosttyKey_GHOSTTY_KEY_F10,
            11 => ffi::GhosttyKey_GHOSTTY_KEY_F11,
            12 => ffi::GhosttyKey_GHOSTTY_KEY_F12,
            _ => return None,
        }),
        KeyCode::Char(c) => ghostty_key_from_char(c, shifted_codepoint),
        _ => None,
    }
}

fn ghostty_key_from_char(c: char, shifted_codepoint: Option<u32>) -> Option<u32> {
    use crate::ghostty::ffi;

    let base = if let Some(shifted) = shifted_codepoint.and_then(char::from_u32) {
        ghostty_unshifted_ascii_pair(shifted).unwrap_or(c)
    } else {
        c
    };

    match base.to_ascii_lowercase() {
        'a' => Some(ffi::GhosttyKey_GHOSTTY_KEY_A),
        'b' => Some(ffi::GhosttyKey_GHOSTTY_KEY_B),
        'c' => Some(ffi::GhosttyKey_GHOSTTY_KEY_C),
        'd' => Some(ffi::GhosttyKey_GHOSTTY_KEY_D),
        'e' => Some(ffi::GhosttyKey_GHOSTTY_KEY_E),
        'f' => Some(ffi::GhosttyKey_GHOSTTY_KEY_F),
        'g' => Some(ffi::GhosttyKey_GHOSTTY_KEY_G),
        'h' => Some(ffi::GhosttyKey_GHOSTTY_KEY_H),
        'i' => Some(ffi::GhosttyKey_GHOSTTY_KEY_I),
        'j' => Some(ffi::GhosttyKey_GHOSTTY_KEY_J),
        'k' => Some(ffi::GhosttyKey_GHOSTTY_KEY_K),
        'l' => Some(ffi::GhosttyKey_GHOSTTY_KEY_L),
        'm' => Some(ffi::GhosttyKey_GHOSTTY_KEY_M),
        'n' => Some(ffi::GhosttyKey_GHOSTTY_KEY_N),
        'o' => Some(ffi::GhosttyKey_GHOSTTY_KEY_O),
        'p' => Some(ffi::GhosttyKey_GHOSTTY_KEY_P),
        'q' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Q),
        'r' => Some(ffi::GhosttyKey_GHOSTTY_KEY_R),
        's' => Some(ffi::GhosttyKey_GHOSTTY_KEY_S),
        't' => Some(ffi::GhosttyKey_GHOSTTY_KEY_T),
        'u' => Some(ffi::GhosttyKey_GHOSTTY_KEY_U),
        'v' => Some(ffi::GhosttyKey_GHOSTTY_KEY_V),
        'w' => Some(ffi::GhosttyKey_GHOSTTY_KEY_W),
        'x' => Some(ffi::GhosttyKey_GHOSTTY_KEY_X),
        'y' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Y),
        'z' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Z),
        '0' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0),
        '1' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_1),
        '2' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_2),
        '3' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_3),
        '4' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_4),
        '5' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_5),
        '6' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_6),
        '7' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_7),
        '8' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_8),
        '9' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_9),
        '`' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKQUOTE),
        '\\' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSLASH),
        '[' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT),
        ']' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT),
        ',' => Some(ffi::GhosttyKey_GHOSTTY_KEY_COMMA),
        '=' => Some(ffi::GhosttyKey_GHOSTTY_KEY_EQUAL),
        '-' => Some(ffi::GhosttyKey_GHOSTTY_KEY_MINUS),
        '.' => Some(ffi::GhosttyKey_GHOSTTY_KEY_PERIOD),
        '\'' => Some(ffi::GhosttyKey_GHOSTTY_KEY_QUOTE),
        ';' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SEMICOLON),
        '/' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SLASH),
        ' ' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SPACE),
        _ => None,
    }
}

fn ghostty_unshifted_ascii_pair(c: char) -> Option<char> {
    Some(match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}
