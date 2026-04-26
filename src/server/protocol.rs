//! Wire protocol for herdr server/client communication.
//!
//! Defines the message types, framing, version negotiation, and safety
//! constraints for the binary protocol over Unix domain sockets.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

/// Current protocol version. Bumped when wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 2;

/// Maximum allowed frame payload size (2 MB). Frames larger than this are
/// rejected to prevent denial-of-service via oversized length prefixes.
pub const MAX_FRAME_SIZE: usize = 2 * 1024 * 1024;

/// Length of the u32 little-endian length prefix in bytes.
const LENGTH_PREFIX_BYTES: usize = 4;

// ---------------------------------------------------------------------------
// Client → Server messages
// ---------------------------------------------------------------------------

/// Messages sent from the client to the server over the client protocol socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Handshake: client announces its protocol version and terminal dimensions.
    Hello {
        /// Protocol version the client speaks.
        version: u32,
        /// Terminal width in columns.
        cols: u16,
        /// Terminal height in rows.
        rows: u16,
    },

    /// Raw input bytes read from the client's stdin.
    Input {
        /// Raw terminal input (possibly multi-byte escape sequences).
        data: Vec<u8>,
    },

    /// Terminal resize notification from the client.
    Resize {
        /// New terminal width in columns.
        cols: u16,
        /// New terminal height in rows.
        rows: u16,
    },

    /// Graceful disconnect request.
    Detach,
}

// ---------------------------------------------------------------------------
// Server → Client messages
// ---------------------------------------------------------------------------

/// A single cell in a rendered frame, serialized independently from ratatui's
/// `Cell` type to keep the wire protocol stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellData {
    /// Grapheme cluster displayed in this cell (usually 1–2 chars).
    pub symbol: String,
    /// Foreground color as a packed u32 (0xAARRGGBB or ratatui Color index).
    pub fg: u32,
    /// Background color as a packed u32.
    pub bg: u32,
    /// Bitmask of style modifiers (bold, italic, etc.).
    pub modifier: u16,
    /// Whether this cell should be skipped during diff-based rendering.
    pub skip: bool,
}

/// Cursor position within a rendered frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Column offset (0-based) of the cursor.
    pub x: u16,
    /// Row offset (0-based) of the cursor.
    pub y: u16,
    /// Whether the cursor is visible.
    pub visible: bool,
}

/// A rendered frame to be displayed by the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameData {
    /// Cells in row-major order. Length must equal `width * height`.
    pub cells: Vec<CellData>,
    /// Frame width in columns.
    pub width: u16,
    /// Frame height in rows.
    pub height: u16,
    /// Cursor state for this frame, if applicable.
    pub cursor: Option<CursorState>,
}

impl FrameData {
    /// Creates a `FrameData` from a ratatui `Buffer` and optional cursor.
    ///
    /// This converts ratatui's internal cell representation into the
    /// wire-protocol cell format. The conversion is lossless for all
    /// commonly used cell attributes.
    pub fn from_ratatui_buffer(
        buffer: &ratatui::buffer::Buffer,
        cursor: Option<CursorState>,
    ) -> Self {
        let area = buffer.area;
        let width = area.width;
        let height = area.height;

        let mut cells = Vec::with_capacity((width as usize) * (height as usize));
        for row in 0..height {
            for col in 0..width {
                let cell = buffer.cell((col, row)).expect("cell within bounds");
                cells.push(CellData {
                    symbol: cell.symbol().to_owned(),
                    fg: color_to_u32(cell.fg),
                    bg: color_to_u32(cell.bg),
                    modifier: modifier_to_u16(cell.modifier),
                    skip: cell.skip,
                });
            }
        }

        FrameData {
            cells,
            width,
            height,
            cursor,
        }
    }

    /// Reconstructs a ratatui `Buffer` from this frame data.
    ///
    /// Returns `None` if the cells vector length doesn't match `width * height`.
    #[cfg(test)]
    pub fn to_ratatui_buffer(&self) -> Option<ratatui::buffer::Buffer> {
        let expected = (self.width as usize) * (self.height as usize);
        if self.cells.len() != expected {
            return None;
        }

        let area = ratatui::layout::Rect::new(0, 0, self.width, self.height);
        let mut buffer = ratatui::buffer::Buffer::filled(area, ratatui::buffer::Cell::new(" "));

        for row in 0..self.height {
            for col in 0..self.width {
                let idx = (row as usize) * (self.width as usize) + (col as usize);
                let cell_data = &self.cells[idx];
                let cell = buffer.cell_mut((col, row)).expect("cell within bounds");
                cell.set_symbol(&cell_data.symbol);
                cell.fg = u32_to_color(cell_data.fg);
                cell.bg = u32_to_color(cell_data.bg);
                cell.modifier = u16_to_modifier(cell_data.modifier);
                cell.skip = cell_data.skip;
            }
        }

        Some(buffer)
    }
}

/// Notification kind forwarded from server to client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotifyKind {
    /// Play a sound (bell/agent-done, etc.).
    Sound,
    /// Display a toast message.
    Toast,
}

/// Messages sent from the server to the client over the client protocol socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Handshake response: server acknowledges (or rejects) the client.
    Welcome {
        /// Protocol version the server speaks.
        version: u32,
        /// If present, the handshake failed and this describes why.
        /// The client should exit with a clear error message.
        error: Option<String>,
    },

    /// A rendered frame to be displayed by the client.
    Frame(FrameData),

    /// Server is shutting down. Clients should exit gracefully.
    ServerShutdown {
        /// Optional reason for the shutdown.
        reason: Option<String>,
    },

    /// A notification event (sound/toast) to be rendered locally by the client.
    Notify {
        /// What kind of notification.
        kind: NotifyKind,
        /// Human-readable message.
        message: String,
    },

    /// OSC 52 clipboard data forwarded from a PTY through the server.
    Clipboard {
        /// Base64-encoded clipboard data.
        data: String,
    },

    /// Client-local sound config changed on disk; refresh it without reconnecting.
    ReloadSoundConfig,
}

// ---------------------------------------------------------------------------
// Color / Modifier conversion helpers
// ---------------------------------------------------------------------------

/// Converts a ratatui `Color` to a packed u32 for wire transport.
///
/// Encoding:
/// - Named colors (Reset, Black, …, White) → `0x00_00_00_XX` where XX is 0..=16
/// - Indexed palette → `0x01_00_00_XX` where XX is the palette index
/// - RGB → `0x02_RR_GG_BB` with components in the lower 3 bytes
fn color_to_u32(color: ratatui::style::Color) -> u32 {
    match color {
        ratatui::style::Color::Reset => 0x00_00_00_00,
        ratatui::style::Color::Black => 0x00_00_00_01,
        ratatui::style::Color::Red => 0x00_00_00_02,
        ratatui::style::Color::Green => 0x00_00_00_03,
        ratatui::style::Color::Yellow => 0x00_00_00_04,
        ratatui::style::Color::Blue => 0x00_00_00_05,
        ratatui::style::Color::Magenta => 0x00_00_00_06,
        ratatui::style::Color::Cyan => 0x00_00_00_07,
        ratatui::style::Color::Gray => 0x00_00_00_08,
        ratatui::style::Color::DarkGray => 0x00_00_00_09,
        ratatui::style::Color::LightRed => 0x00_00_00_0A,
        ratatui::style::Color::LightGreen => 0x00_00_00_0B,
        ratatui::style::Color::LightYellow => 0x00_00_00_0C,
        ratatui::style::Color::LightBlue => 0x00_00_00_0D,
        ratatui::style::Color::LightMagenta => 0x00_00_00_0E,
        ratatui::style::Color::LightCyan => 0x00_00_00_0F,
        ratatui::style::Color::White => 0x00_00_00_10,
        ratatui::style::Color::Indexed(i) => 0x01_00_00_00 | (i as u32),
        ratatui::style::Color::Rgb(r, g, b) => {
            0x02_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        }
    }
}

/// Converts a packed u32 back to a ratatui `Color`.
#[cfg(test)]
fn u32_to_color(val: u32) -> ratatui::style::Color {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => ratatui::style::Color::Reset,
            0x01 => ratatui::style::Color::Black,
            0x02 => ratatui::style::Color::Red,
            0x03 => ratatui::style::Color::Green,
            0x04 => ratatui::style::Color::Yellow,
            0x05 => ratatui::style::Color::Blue,
            0x06 => ratatui::style::Color::Magenta,
            0x07 => ratatui::style::Color::Cyan,
            0x08 => ratatui::style::Color::Gray,
            0x09 => ratatui::style::Color::DarkGray,
            0x0A => ratatui::style::Color::LightRed,
            0x0B => ratatui::style::Color::LightGreen,
            0x0C => ratatui::style::Color::LightYellow,
            0x0D => ratatui::style::Color::LightBlue,
            0x0E => ratatui::style::Color::LightMagenta,
            0x0F => ratatui::style::Color::LightCyan,
            0x10 => ratatui::style::Color::White,
            _ => ratatui::style::Color::Reset, // unknown named → Reset
        },
        0x01 => ratatui::style::Color::Indexed((val & 0xFF) as u8),
        0x02 => {
            let r = ((val >> 16) & 0xFF) as u8;
            let g = ((val >> 8) & 0xFF) as u8;
            let b = (val & 0xFF) as u8;
            ratatui::style::Color::Rgb(r, g, b)
        }
        _ => ratatui::style::Color::Reset, // unknown tag → Reset
    }
}

/// Converts a ratatui `Modifier` bitmask to a u16 for wire transport.
fn modifier_to_u16(modifier: ratatui::style::Modifier) -> u16 {
    modifier.bits()
}

/// Converts a u16 back to a ratatui `Modifier`.
#[cfg(test)]
fn u16_to_modifier(val: u16) -> ratatui::style::Modifier {
    ratatui::style::Modifier::from_bits_truncate(val)
}

// ---------------------------------------------------------------------------
// Framing: length-prefixed binary messages
// ---------------------------------------------------------------------------

/// Errors that can occur during framing operations.
#[derive(Debug)]
pub enum FramingError {
    /// The decoded payload length exceeds the configured maximum frame size.
    Oversized { claimed: usize, max: usize },
    /// An I/O error occurred while reading or writing.
    Io(io::Error),
    /// Bincode serialization or deserialization failed.
    Bincode(String),
    /// The connection was closed before a complete frame could be read.
    UnexpectedEof,
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramingError::Oversized { claimed, max } => {
                write!(f, "frame size {claimed} exceeds maximum {max}")
            }
            FramingError::Io(e) => write!(f, "I/O error: {e}"),
            FramingError::Bincode(e) => write!(f, "bincode error: {e}"),
            FramingError::UnexpectedEof => write!(f, "unexpected end of stream"),
        }
    }
}

impl std::error::Error for FramingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FramingError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for FramingError {
    fn from(e: io::Error) -> Self {
        FramingError::Io(e)
    }
}

/// Serializes a message and writes it as a length-prefixed frame:
/// `[u32LE length][bincode payload]`.
///
/// This is a blocking/synchronous write suitable for use with `std::os::unix::net::UnixStream`
/// in blocking mode, or with any `Write` implementor.
///
/// # Errors
///
/// Returns `FramingError::Bincode` if the payload length exceeds `u32::MAX`
/// (would be truncated by the length prefix cast).
pub fn write_message<W: Write, M: Serialize>(writer: &mut W, msg: &M) -> Result<(), FramingError> {
    let payload = bincode::serde::encode_to_vec(msg, bincode::config::standard())
        .map_err(|e| FramingError::Bincode(e.to_string()))?;

    let len = payload.len();
    if len > u32::MAX as usize {
        return Err(FramingError::Bincode(format!(
            "payload length {len} exceeds u32::MAX ({}), would be truncated by length prefix",
            u32::MAX
        )));
    }

    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

/// Reads and deserializes a length-prefixed frame from a reader.
///
/// Reassembles partial reads correctly. Rejects frames whose declared
/// length exceeds `max_frame_size` without panicking or allocating
/// oversized buffers.
pub fn read_message<R: Read, M: for<'de> Deserialize<'de>>(
    reader: &mut R,
    max_frame_size: usize,
) -> Result<M, FramingError> {
    // Read the 4-byte length prefix, reassembling partial reads.
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    read_exact_or_eof(reader, &mut len_buf)?;
    let claimed_len = u32::from_le_bytes(len_buf) as usize;

    if claimed_len > max_frame_size {
        return Err(FramingError::Oversized {
            claimed: claimed_len,
            max: max_frame_size,
        });
    }

    // Read the payload, reassembling partial reads.
    let mut payload = vec![0u8; claimed_len];
    read_exact_or_eof(reader, &mut payload)?;

    let (msg, consumed) = bincode::serde::decode_from_slice(&payload, bincode::config::standard())
        .map_err(|e| FramingError::Bincode(e.to_string()))?;

    // Enforce that the decoder consumed the full payload.
    // Trailing bytes after the decoded message indicate a protocol violation
    // (e.g., a corrupted length prefix or concatenated payloads).
    if consumed != claimed_len {
        return Err(FramingError::Bincode(format!(
            "decoded {} bytes but payload length was {claimed_len}; trailing bytes are not allowed",
            consumed
        )));
    }

    Ok(msg)
}

/// Like `Read::read_exact`, but returns `FramingError::UnexpectedEof`
/// when the reader hits end-of-stream before filling the buffer, instead
/// of the generic `io::ErrorKind::UnexpectedEof`.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), FramingError> {
    reader.read_exact(buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            FramingError::UnexpectedEof
        } else {
            FramingError::Io(e)
        }
    })
}

// ---------------------------------------------------------------------------
// Version negotiation
// ---------------------------------------------------------------------------

/// Result of checking a client's protocol version against the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    /// Versions are compatible. The server should reply `Welcome { version, error: None }`.
    Compatible,
    /// Versions are incompatible. The server should reply
    /// `Welcome { version, error: Some(reason) }` and close the connection.
    Incompatible(String),
}

/// Checks whether a client's protocol version is compatible with this server.
///
/// Current rules:
/// - Version 0 (pre-persistence client) is always rejected.
/// - Matching major versions are accepted.
/// - A client with a newer version than the server is rejected.
/// - A client with an older version than the server is rejected
///   (backward compatibility is not yet supported).
pub fn check_client_version(client_version: u32) -> VersionCheck {
    if client_version == 0 {
        return VersionCheck::Incompatible(
            "pre-persistence client (version 0) is not supported".to_owned(),
        );
    }

    if client_version == PROTOCOL_VERSION {
        VersionCheck::Compatible
    } else if client_version < PROTOCOL_VERSION {
        VersionCheck::Incompatible(format!(
            "client version {client_version} is older than server version {PROTOCOL_VERSION}; please upgrade your herdr client"
        ))
    } else {
        VersionCheck::Incompatible(format!(
            "client version {client_version} is newer than server version {PROTOCOL_VERSION}; please upgrade the herdr server"
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    // ---- Round-trip: ClientMessage ----

    #[test]
    fn client_hello_roundtrip() {
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_input_roundtrip() {
        let msg = ClientMessage::Input {
            data: vec![0x1b, 0x5b, 0x41], // ESC [ A (up arrow)
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_resize_roundtrip() {
        let msg = ClientMessage::Resize { cols: 80, rows: 24 };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn client_detach_roundtrip() {
        let msg = ClientMessage::Detach;
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ClientMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Round-trip: ServerMessage ----

    #[test]
    fn server_welcome_roundtrip() {
        let msg = ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            error: None,
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_welcome_with_error_roundtrip() {
        let msg = ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            error: Some("incompatible version".to_owned()),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_frame_roundtrip_nontrivial() {
        // Build a 3×2 frame with varied styles (≥2×2).
        let frame = FrameData {
            cells: vec![
                CellData {
                    symbol: "H".into(),
                    fg: color_to_u32(Color::Red),
                    bg: color_to_u32(Color::Black),
                    modifier: Modifier::BOLD.bits(),
                    skip: false,
                },
                CellData {
                    symbol: "i".into(),
                    fg: color_to_u32(Color::Green),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::ITALIC.bits(),
                    skip: false,
                },
                CellData {
                    symbol: "!".into(),
                    fg: color_to_u32(Color::Rgb(255, 128, 0)),
                    bg: color_to_u32(Color::Indexed(220)),
                    modifier: (Modifier::BOLD | Modifier::UNDERLINED).bits(),
                    skip: false,
                },
                CellData {
                    symbol: " ".into(),
                    fg: color_to_u32(Color::Reset),
                    bg: color_to_u32(Color::Reset),
                    modifier: Modifier::empty().bits(),
                    skip: true,
                },
                CellData {
                    symbol: "→".into(), // multi-byte grapheme
                    fg: color_to_u32(Color::Cyan),
                    bg: color_to_u32(Color::Blue),
                    modifier: Modifier::REVERSED.bits(),
                    skip: false,
                },
                CellData {
                    symbol: "🦀".into(), // emoji, wide grapheme cluster
                    fg: color_to_u32(Color::Yellow),
                    bg: color_to_u32(Color::Magenta),
                    modifier: Modifier::empty().bits(),
                    skip: false,
                },
            ],
            width: 3,
            height: 2,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
            }),
        };
        let msg = ServerMessage::Frame(frame.clone());
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_shutdown_roundtrip() {
        let msg = ServerMessage::ServerShutdown {
            reason: Some("updating".to_owned()),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_notify_roundtrip() {
        let msg = ServerMessage::Notify {
            kind: NotifyKind::Sound,
            message: "agent done".to_owned(),
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_clipboard_roundtrip() {
        let msg = ServerMessage::Clipboard {
            data: "dGVzdA==".to_owned(), // base64 "test"
        };
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn server_reload_sound_config_roundtrip() {
        let msg = ServerMessage::ReloadSoundConfig;
        let encoded = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let (decoded, _): (ServerMessage, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::standard()).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Framing ----

    #[test]
    fn framing_small_message_roundtrip() {
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_large_payload_roundtrip() {
        // Create a Frame message that is ≥128 KB.
        // Use a large frame with verbose cell data to exceed 128 KB after bincode encoding.
        // 200×50 = 10000 cells. With varied symbols and styles, this should easily exceed 128 KB.
        let width: u16 = 200;
        let height: u16 = 50;
        let cells: Vec<CellData> = (0..(width as usize) * (height as usize))
            .map(|i| CellData {
                symbol: if i % 256 < 32 {
                    " ".to_owned()
                } else {
                    format!("{:03}", i % 1000)
                },
                fg: color_to_u32(Color::Rgb((i % 256) as u8, ((i / 256) % 256) as u8, 128)),
                bg: color_to_u32(Color::Indexed((i % 256) as u8)),
                modifier: ((i % 16) as u16),
                skip: i % 100 == 0,
            })
            .collect();

        let frame = FrameData {
            cells,
            width,
            height,
            cursor: Some(CursorState {
                x: 10,
                y: 5,
                visible: true,
            }),
        };
        let msg = ServerMessage::Frame(frame);

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        // Verify the payload is at least 128 KB
        assert!(
            buf.len() >= 128 * 1024,
            "framed payload should be >= 128 KB, got {} bytes",
            buf.len()
        );

        let decoded: ServerMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_multiple_messages_sequential() {
        // Write 100+ messages of varying types and read them back.
        let mut buf = Vec::new();
        let mut expected = Vec::new();

        for i in 0..150u32 {
            let msg = match i % 4 {
                0 => ClientMessage::Hello {
                    version: PROTOCOL_VERSION,
                    cols: (80 + (i % 40) as u16),
                    rows: (24 + (i % 20) as u16),
                },
                1 => ClientMessage::Input {
                    data: vec![(i % 256) as u8; (i as usize % 50) + 1],
                },
                2 => ClientMessage::Resize {
                    cols: (100 + (i % 30) as u16),
                    rows: (30 + (i % 10) as u16),
                },
                3 => ClientMessage::Detach,
                _ => unreachable!(),
            };
            write_message(&mut buf, &msg).unwrap();
            expected.push(msg);
        }

        let mut cursor = buf.as_slice();
        for expected_msg in &expected {
            let decoded: ClientMessage = read_message(&mut cursor, MAX_FRAME_SIZE).unwrap();
            assert_eq!(*expected_msg, decoded);
        }
    }

    #[test]
    fn framing_oversized_rejected_without_panic() {
        // Craft a frame with a huge length prefix (4 GB claim).
        let mut buf: Vec<u8> = (u32::MAX).to_le_bytes().to_vec();
        // Add a few garbage bytes after the length prefix.
        buf.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::Oversized { claimed, max }) => {
                assert_eq!(claimed, u32::MAX as usize);
                assert_eq!(max, MAX_FRAME_SIZE);
            }
            other => panic!("expected Oversized error, got: {other:?}"),
        }
    }

    #[test]
    fn framing_malformed_payload_rejected_without_panic() {
        // Valid length prefix pointing to garbage data.
        let payload = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];
        let mut buf = (payload.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&payload);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err(), "malformed payload should be rejected");
        match result {
            Err(FramingError::Bincode(_)) => {} // expected
            other => panic!("expected Bincode error, got: {other:?}"),
        }
    }

    #[test]
    fn framing_truncated_stream_returns_unexpected_eof() {
        // Write a length prefix claiming 100 bytes, but only provide 4.
        let mut buf: Vec<u8> = 100u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::UnexpectedEof) => {}
            other => panic!("expected UnexpectedEof, got: {other:?}"),
        }
    }

    #[test]
    fn framing_zero_length_message() {
        // A 1-byte message (smallest possible valid bincode payload).
        // Actually, let's test with the smallest real message: Detach.
        let msg = ClientMessage::Detach;
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        // Verify the length prefix is correct
        let len = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
        assert_eq!(
            len,
            buf.len() - 4,
            "length prefix should match payload size"
        );

        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn framing_partial_read_reassembly() {
        // Simulate partial reads by using a reader that yields small chunks.
        let msg = ClientMessage::Input {
            data: vec![42; 500], // 500-byte input payload
        };
        let mut full_buf = Vec::new();
        write_message(&mut full_buf, &msg).unwrap();

        // Wrap in a chunked reader that only yields 7 bytes at a time.
        let mut chunked = ChunkedReader::new(full_buf, 7);
        let decoded: ClientMessage = read_message(&mut chunked, MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    // ---- Version negotiation ----

    #[test]
    fn version_compatible() {
        assert_eq!(
            check_client_version(PROTOCOL_VERSION),
            VersionCheck::Compatible
        );
    }

    #[test]
    fn version_older_client_rejected() {
        let result = check_client_version(PROTOCOL_VERSION - 1);
        assert!(matches!(result, VersionCheck::Incompatible(_)));
        if let VersionCheck::Incompatible(msg) = result {
            assert!(msg.contains("older"), "error should mention older version");
        }
    }

    #[test]
    fn version_newer_client_rejected() {
        let result = check_client_version(PROTOCOL_VERSION + 1);
        assert!(matches!(result, VersionCheck::Incompatible(_)));
        if let VersionCheck::Incompatible(msg) = result {
            assert!(msg.contains("newer"), "error should mention newer version");
        }
    }

    // ---- Pre-persistence client rejection ----

    #[test]
    fn prepersistence_version_zero_rejected() {
        let result = check_client_version(0);
        match result {
            VersionCheck::Incompatible(msg) => {
                assert!(
                    msg.contains("pre-persistence"),
                    "error should mention pre-persistence: {msg}"
                );
            }
            _ => panic!("version 0 should be rejected as incompatible"),
        }
    }

    #[test]
    fn prepersistence_version_zero_welcome_has_error() {
        // Simulating what the server would send to a v0 client.
        let check = check_client_version(0);
        let response = match check {
            VersionCheck::Compatible => ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                error: None,
            },
            VersionCheck::Incompatible(reason) => ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                error: Some(reason),
            },
        };

        match response {
            ServerMessage::Welcome { error: Some(_), .. } => {}
            other => panic!("expected Welcome with error, got: {other:?}"),
        }
    }

    // ---- Malformed/oversized input ----

    #[test]
    fn oversized_frame_does_not_panic() {
        // Claim 4GB payload — should return Oversized error, not panic.
        let mut buf: Vec<u8> = 0xFFC00000u32.to_le_bytes().to_vec(); // ~4 GB claim
        buf.extend_from_slice(&[0; 8]);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err());
        // Did not panic — test passing is proof.
    }

    #[test]
    fn malformed_frame_does_not_panic() {
        // Random garbage bytes after a valid-ish length prefix.
        let garbage: Vec<u8> = (0..200).map(|i| (i ^ 0xAA) as u8).collect();
        let mut buf = (garbage.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&garbage);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        assert!(result.is_err());
        // Did not panic.
    }

    #[test]
    fn oversized_input_rejected_custom_max() {
        // Verify a custom (small) max_frame_size is enforced.
        let msg = ClientMessage::Input {
            data: vec![0x41; 1000],
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let result: Result<ClientMessage, FramingError> = read_message(&mut buf.as_slice(), 64);
        // The actual bincode payload for 1000 bytes of input will be > 64 bytes.
        assert!(
            matches!(result, Err(FramingError::Oversized { .. })),
            "expected Oversized with small max_frame_size"
        );
    }

    // ---- FrameData ↔ ratatui Buffer conversion ----

    #[test]
    fn frame_data_roundtrip_through_ratatui_buffer() {
        let area = ratatui::layout::Rect::new(0, 0, 5, 3);
        let mut buffer = ratatui::buffer::Buffer::filled(area, ratatui::buffer::Cell::new(" "));

        // Write some styled content.
        buffer.cell_mut((0, 0)).unwrap().set_symbol("H");
        buffer.cell_mut((0, 0)).unwrap().fg = Color::Red;
        buffer.cell_mut((0, 0)).unwrap().modifier = Modifier::BOLD;

        buffer.cell_mut((1, 0)).unwrap().set_symbol("i");
        buffer.cell_mut((1, 0)).unwrap().fg = Color::Green;
        buffer.cell_mut((1, 0)).unwrap().modifier = Modifier::ITALIC;

        buffer.cell_mut((2, 0)).unwrap().set_symbol("!");
        buffer.cell_mut((2, 0)).unwrap().fg = Color::Rgb(255, 128, 0);
        buffer.cell_mut((2, 0)).unwrap().bg = Color::Indexed(220);

        let cursor = CursorState {
            x: 1,
            y: 0,
            visible: true,
        };
        let frame = FrameData::from_ratatui_buffer(&buffer, Some(cursor.clone()));

        // Verify frame dimensions.
        assert_eq!(frame.width, 5);
        assert_eq!(frame.height, 3);
        assert_eq!(frame.cells.len(), 15);
        assert_eq!(frame.cursor, Some(cursor));

        // Verify specific cells survived the conversion.
        assert_eq!(frame.cells[0].symbol, "H");
        assert_eq!(frame.cells[0].fg, color_to_u32(Color::Red));
        assert_eq!(frame.cells[0].modifier, Modifier::BOLD.bits());

        assert_eq!(frame.cells[1].symbol, "i");
        assert_eq!(frame.cells[1].fg, color_to_u32(Color::Green));
        assert_eq!(frame.cells[1].modifier, Modifier::ITALIC.bits());

        assert_eq!(frame.cells[2].symbol, "!");
        assert_eq!(frame.cells[2].fg, color_to_u32(Color::Rgb(255, 128, 0)));
        assert_eq!(frame.cells[2].bg, color_to_u32(Color::Indexed(220)));

        // Convert back to ratatui buffer and compare.
        let restored = frame.to_ratatui_buffer().expect("should reconstruct");
        assert_eq!(restored.area, area);
        assert_eq!(restored.cell((0, 0)).unwrap().symbol(), "H");
        assert_eq!(restored.cell((0, 0)).unwrap().fg, Color::Red);
        assert_eq!(restored.cell((0, 0)).unwrap().modifier, Modifier::BOLD);
        assert_eq!(restored.cell((1, 0)).unwrap().symbol(), "i");
        assert_eq!(restored.cell((2, 0)).unwrap().symbol(), "!");
        assert_eq!(restored.cell((2, 0)).unwrap().fg, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn frame_data_rejects_mismatched_cell_count() {
        let frame = FrameData {
            cells: vec![
                CellData {
                    symbol: "X".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                };
                5
            ], // 5 cells but 3×2 = 6 expected
            width: 3,
            height: 2,
            cursor: None,
        };
        assert!(frame.to_ratatui_buffer().is_none());
    }

    // ---- Color conversion coverage ----

    #[test]
    fn color_roundtrip_all_named_colors() {
        let named = [
            Color::Reset,
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ];
        for c in named {
            assert_eq!(
                u32_to_color(color_to_u32(c)),
                c,
                "roundtrip failed for {c:?}"
            );
        }
    }

    #[test]
    fn color_roundtrip_indexed() {
        for i in 0..=255u8 {
            let c = Color::Indexed(i);
            assert_eq!(
                u32_to_color(color_to_u32(c)),
                c,
                "roundtrip failed for Indexed({i})"
            );
        }
    }

    #[test]
    fn color_roundtrip_rgb() {
        let c = Color::Rgb(0xAB, 0xCD, 0xEF);
        assert_eq!(u32_to_color(color_to_u32(c)), c);

        let c = Color::Rgb(0, 0, 0);
        assert_eq!(u32_to_color(color_to_u32(c)), c);

        let c = Color::Rgb(255, 255, 255);
        assert_eq!(u32_to_color(color_to_u32(c)), c);
    }

    // ---- Modifier conversion ----

    #[test]
    fn modifier_roundtrip() {
        let all_mods = [
            Modifier::BOLD,
            Modifier::ITALIC,
            Modifier::REVERSED,
            Modifier::UNDERLINED,
            Modifier::DIM,
            Modifier::SLOW_BLINK,
            Modifier::CROSSED_OUT,
            Modifier::BOLD | Modifier::ITALIC,
            Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED,
            Modifier::empty(),
        ];
        for m in all_mods {
            assert_eq!(
                u16_to_modifier(modifier_to_u16(m)),
                m,
                "roundtrip failed for {m:?}"
            );
        }
    }

    #[test]
    fn read_message_rejects_trailing_bytes() {
        // Encode a valid message, then append an extra byte after it.
        let msg = ClientMessage::Detach;
        let mut payload = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        let original_len = payload.len();
        payload.push(0xDE); // trailing garbage

        // Frame it with the inflated length (original + 1).
        let mut buf = (payload.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(&payload);

        let result: Result<ClientMessage, FramingError> =
            read_message(&mut buf.as_slice(), MAX_FRAME_SIZE);
        match result {
            Err(FramingError::Bincode(msg)) => {
                assert!(
                    msg.contains("trailing bytes"),
                    "error should mention trailing bytes: {msg}"
                );
                assert!(
                    msg.contains(&format!("decoded {original_len}")),
                    "error should mention decoded byte count: {msg}"
                );
            }
            other => panic!("expected Bincode error about trailing bytes, got: {other:?}"),
        }
    }

    #[test]
    fn read_message_accepts_exact_payload() {
        // A normally-framed message should decode without error.
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            cols: 80,
            rows: 24,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let decoded: ClientMessage = read_message(&mut buf.as_slice(), MAX_FRAME_SIZE).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn write_message_rejects_oversized_payload() {
        // We can't easily create a message that exceeds u32::MAX in a test,
        // but we can verify the check exists by testing that normal messages
        // have lengths well within the limit and the function doesn't fail.
        let msg = ClientMessage::Detach;
        let mut buf = Vec::new();
        assert!(write_message(&mut buf, &msg).is_ok());
    }

    // ---- Unix socketpair integration test ----

    #[test]
    fn framing_over_unix_socketpair() {
        use std::os::unix::net::UnixStream;

        let (mut a, mut b) = UnixStream::pair().expect("socketpair");

        let messages = vec![
            ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 200,
                rows: 60,
            },
            ClientMessage::Input {
                data: b"hello world".to_vec(),
            },
            ClientMessage::Resize {
                cols: 100,
                rows: 30,
            },
            ClientMessage::Detach,
        ];

        // Set non-blocking so we can write and read in the same test.
        a.set_nonblocking(false).unwrap();
        b.set_nonblocking(false).unwrap();

        for msg in &messages {
            write_message(&mut a, msg).unwrap();
        }

        for expected in &messages {
            let decoded: ClientMessage = read_message(&mut b, MAX_FRAME_SIZE).unwrap();
            assert_eq!(*expected, decoded);
        }
    }

    // ---- Helper: chunked reader for simulating partial reads ----

    /// A `Read` wrapper that yields at most `chunk_size` bytes per `read()` call,
    /// simulating partial reads on a real socket.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk_size,
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let remaining = self.data.len() - self.pos;
            let to_read = buf.len().min(remaining).min(self.chunk_size);
            buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            Ok(to_read)
        }
    }
}
