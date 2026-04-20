use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalKey {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: crossterm::event::KeyEventKind,
    pub shifted_codepoint: Option<u32>,
}

impl TerminalKey {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            shifted_codepoint: None,
        }
    }

    pub fn with_kind(mut self, kind: crossterm::event::KeyEventKind) -> Self {
        self.kind = kind;
        self
    }

    #[allow(dead_code)] // Reserved for the upcoming raw input parser to preserve shifted/base key pairs.
    pub fn with_shifted_codepoint(mut self, shifted_codepoint: u32) -> Self {
        self.shifted_codepoint = Some(shifted_codepoint);
        self
    }

    pub fn as_key_event(self) -> KeyEvent {
        KeyEvent::new_with_kind(self.code, self.modifiers, self.kind)
    }
}

impl From<KeyEvent> for TerminalKey {
    fn from(value: KeyEvent) -> Self {
        Self::new(value.code, value.modifiers).with_kind(value.kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocol {
    Legacy,
    Kitty { flags: u16 },
}

impl KeyboardProtocol {
    pub fn from_kitty_flags(flags: u16) -> Self {
        if flags == 0 {
            Self::Legacy
        } else {
            Self::Kitty { flags }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocolMode {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

impl MouseProtocolMode {
    pub fn reporting_enabled(self) -> bool {
        self != Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    Default,
    Utf8,
    Sgr,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
