mod encode;
mod model;
mod parse;

#[cfg(test)]
pub use encode::encode_key;
#[allow(unused_imports)]
pub use encode::{
    encode_cursor_key, encode_mouse_button, encode_mouse_scroll, encode_terminal_key,
};
pub use model::{KeyboardProtocol, MouseProtocolEncoding, MouseProtocolMode, TerminalKey};
pub use parse::parse_terminal_key_sequence;

#[cfg(test)]
mod tests;
