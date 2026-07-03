use std::io::Read;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Parse raw terminal input bytes into a list of `RawInputEvent`s.
///
/// This is used by the headless server to route client input through the
/// same parsing pipeline that the monolithic binary uses for stdin.
/// Incomplete sequences at the end of the buffer are flushed as best-effort
/// (same logic as the live input reader).
#[allow(dead_code)]
pub fn parse_raw_input_bytes(data: &[u8]) -> Vec<RawInputEvent> {
    // Delegate to the sync version which actually works.
    parse_raw_input_bytes_sync(data)
}

/// A raw input event paired with the byte range it consumed from the original buffer.
#[cfg(test)]
#[derive(Debug)]
pub struct RawInputEventWithRange {
    /// The parsed event.
    pub event: RawInputEvent,
    /// Byte offset where this event starts in the original buffer.
    pub start: usize,
    /// Number of bytes this event consumed from the original buffer.
    /// For events generated from flushed incomplete bytes, `len` may be 0
    /// (synthetic events that don't map to original bytes).
    pub len: usize,
}

/// Parse raw terminal input bytes into a list of `RawInputEventWithRange`s (synchronous version).
///
/// Unlike `parse_raw_input_bytes_sync`, this preserves the byte offset for each
/// event, allowing callers to write only the specific bytes for each event
/// instead of the entire input buffer.
#[cfg(test)]
pub fn parse_raw_input_bytes_with_ranges(data: &[u8]) -> Vec<RawInputEventWithRange> {
    let mut buffer = data.to_vec();
    let mut events = Vec::new();
    let mut offset = 0usize;

    while let Some((event, consumed)) = extract_one_event(&buffer) {
        buffer.drain(..consumed);
        events.push(RawInputEventWithRange {
            event,
            start: offset,
            len: consumed,
        });
        offset += consumed;
    }

    // Flush remaining incomplete bytes.
    if !buffer.is_empty() {
        if buffer.as_slice() == [ESC] {
            events.push(RawInputEventWithRange {
                event: RawInputEvent::Key(TerminalKey::new(
                    crossterm::event::KeyCode::Esc,
                    KeyModifiers::empty(),
                )),
                start: offset,
                len: 1,
            });
        } else if matches!(
            control_string(&buffer),
            Some(ControlString::Incomplete { .. })
        ) {
            return events;
        } else if let Ok(text) = std::str::from_utf8(&buffer) {
            if let Some(key) = parse_terminal_key_sequence(text) {
                events.push(RawInputEventWithRange {
                    event: RawInputEvent::Key(key),
                    start: offset,
                    len: buffer.len(),
                });
            }
        }
    }

    events
}

/// Parse raw terminal input bytes into a list of `RawInputEvent`s (synchronous version).
///
/// Unlike `parse_raw_input_bytes`, this directly extracts events without
/// going through a channel, making it suitable for synchronous use.
pub fn parse_raw_input_bytes_sync(data: &[u8]) -> Vec<RawInputEvent> {
    let mut framer = RawInputFramer::default();
    let mut events = framer.push(data);
    events.extend(framer.flush_timeout());
    events
}

#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

use crate::input::{parse_terminal_key_sequence, TerminalKey};
use crate::terminal_theme::{
    parse_default_color_response, DefaultColorKind, HostAppearance, RgbColor,
};

const ESC: u8 = 0x1b;
pub(crate) const RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS: i32 = 10;
#[cfg(any(not(windows), test))]
pub(crate) const MOUSE_ACTIVE_LONE_ESCAPE_FLUSH_TIMEOUT_MS: i32 = 150;
pub(crate) const GHOSTTY_COLOR_SCHEME_DARK_REPORT: &[u8] = b"\x1b[?997;1n";
pub(crate) const GHOSTTY_COLOR_SCHEME_LIGHT_REPORT: &[u8] = b"\x1b[?997;2n";
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug)]
pub enum RawInputEvent {
    Key(TerminalKey),
    Paste(String),
    Mouse(MouseEvent),
    OuterFocusGained,
    OuterFocusLost,
    HostDefaultColor {
        kind: DefaultColorKind,
        color: RgbColor,
    },
    HostColorSchemeChanged(HostAppearance),
    Unsupported,
}

#[derive(Default)]
pub(crate) struct RawInputFramer {
    byte_framer: RawInputByteFramer,
}

impl RawInputFramer {
    pub(crate) fn push(&mut self, data: &[u8]) -> Vec<RawInputEvent> {
        Self::events_from_chunks(self.byte_framer.push(data))
    }

    pub(crate) fn host_color_query_sent(&mut self) {
        self.byte_framer.host_color_query_sent();
    }

    pub(crate) fn enable_host_color_scheme_change_tracking(&mut self) {
        self.byte_framer.enable_host_color_scheme_change_tracking();
    }

    pub(crate) fn has_pending_input(&self) -> bool {
        self.byte_framer.has_pending_input()
    }

    #[cfg(any(windows, test))]
    pub(crate) fn has_pending_bracketed_paste(&self) -> bool {
        self.byte_framer.has_pending_bracketed_paste()
    }

    pub(crate) fn flush_timeout(&mut self) -> Vec<RawInputEvent> {
        Self::events_from_chunks(self.byte_framer.flush_timeout())
    }

    fn events_from_chunks(chunks: Vec<Vec<u8>>) -> Vec<RawInputEvent> {
        chunks
            .into_iter()
            .filter_map(|chunk| {
                if chunk.as_slice() == [ESC] {
                    return Some(RawInputEvent::Key(TerminalKey::new(
                        crossterm::event::KeyCode::Esc,
                        KeyModifiers::empty(),
                    )));
                }
                extract_one_event(&chunk).map(|(event, _consumed)| {
                    tracing::debug!(raw_bytes = ?chunk, event = ?event, "raw input event parsed");
                    event
                })
            })
            .collect()
    }
}

#[derive(Default)]
pub(crate) struct RawInputByteFramer {
    buffer: Vec<u8>,
    discard_until: Option<ControlStringFamily>,
    discarded_tail_bytes: usize,
    lone_escape_recently_flushed: bool,
    host_color_replies_awaited: u8,
    held_pending_color_esc: bool,
    host_color_scheme_change_tracking: bool,
}

const HOST_COLOR_QUERY_REPLIES: u8 = 2;
const MAX_ORPHANED_SGR_MOUSE_TAIL_BYTES: usize = 32;

impl RawInputByteFramer {
    pub(crate) fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(data);
        self.drain_available_chunks()
    }

    /// Hold a lone trailing ESC for one idle flush so an OSC 10/11 reply split
    /// at its ESC introducer stitches back together instead of leaking (#549).
    pub(crate) fn host_color_query_sent(&mut self) {
        self.host_color_replies_awaited = HOST_COLOR_QUERY_REPLIES;
        self.held_pending_color_esc = false;
    }

    pub(crate) fn enable_host_color_scheme_change_tracking(&mut self) {
        self.host_color_scheme_change_tracking = true;
    }

    pub(crate) fn has_pending_input(&self) -> bool {
        !self.buffer.is_empty()
    }

    #[cfg(any(not(windows), test))]
    pub(crate) fn has_pending_lone_escape(&self) -> bool {
        self.buffer.as_slice() == [ESC]
    }

    #[cfg(any(windows, test))]
    pub(crate) fn has_pending_bracketed_paste(&self) -> bool {
        self.buffer.starts_with(BRACKETED_PASTE_START)
            && find_subsequence(&self.buffer, BRACKETED_PASTE_END).is_none()
    }

    pub(crate) fn flush_timeout(&mut self) -> Vec<Vec<u8>> {
        let mut chunks = self.drain_available_chunks();

        if let Some(family) = self.discard_until {
            let keep_split_st = self.buffer.last() == Some(&ESC);
            let keep_discarding = plausible_control_string_tail(family, &self.buffer);
            self.discarded_tail_bytes = self.discarded_tail_bytes.saturating_add(self.buffer.len());
            self.buffer.clear();
            if keep_discarding && self.discarded_tail_bytes <= MAX_DISCARDED_CONTROL_TAIL_BYTES {
                if keep_split_st {
                    self.buffer.push(ESC);
                }
            } else {
                self.discard_until = None;
                self.discarded_tail_bytes = 0;
            }
            return chunks;
        }

        if self.buffer.is_empty() {
            return chunks;
        }

        if self.lone_escape_recently_flushed && self.buffer.starts_with(b"[<") {
            tracing::debug!(
                len = self.buffer.len(),
                "discarding incomplete orphaned SGR mouse tail after input timeout"
            );
            discard_or_buffer_orphaned_sgr_mouse_tail(
                &mut self.buffer,
                &mut self.discard_until,
                &mut self.discarded_tail_bytes,
            );
            self.lone_escape_recently_flushed = false;
            return chunks;
        }

        if self.buffer.starts_with(BRACKETED_PASTE_START)
            && find_subsequence(&self.buffer, BRACKETED_PASTE_END).is_none()
        {
            tracing::trace!(
                len = self.buffer.len(),
                "waiting for bracketed paste terminator"
            );
            return chunks;
        }

        if starts_with_incomplete_default_color_response(&self.buffer) {
            tracing::trace!(
                len = self.buffer.len(),
                "waiting for host color response terminator"
            );
            return chunks;
        }

        if starts_with_incomplete_host_color_scheme_report(&self.buffer) {
            tracing::debug!(
                len = self.buffer.len(),
                "discarding incomplete host color scheme report after input timeout"
            );
            self.discard_until = Some(ControlStringFamily::HostColorSchemeCsi);
            self.discarded_tail_bytes = 0;
            self.buffer.clear();
            return chunks;
        }

        if let Some(ControlString::Incomplete { family }) = control_string(&self.buffer) {
            tracing::debug!(
                len = self.buffer.len(),
                "discarding incomplete host control string after input timeout"
            );
            // This intentionally gives host control replies precedence over legacy
            // Alt forms like Alt+] after timeout, so later reply tails cannot leak.
            self.discard_until = Some(family);
            self.discarded_tail_bytes = 0;
            self.buffer.clear();
            return chunks;
        }

        if self.buffer.as_slice() == [ESC] {
            if self.host_color_replies_awaited > 0 && !self.held_pending_color_esc {
                self.held_pending_color_esc = true;
                tracing::trace!("holding lone escape one flush while awaiting host color reply");
                return chunks;
            }
            // No continuation arrived; give up the window so Escape is not delayed again.
            self.host_color_replies_awaited = 0;
            self.held_pending_color_esc = false;
            tracing::warn!(
                bytes = ?self.buffer,
                "flushing lone escape after input timeout; if this follows an alt chord or focus switch it may reach the pane as plain esc"
            );
            self.lone_escape_recently_flushed = true;
            chunks.push(std::mem::take(&mut self.buffer));
            return chunks;
        }

        if let Ok(text) = std::str::from_utf8(&self.buffer) {
            if parse_terminal_key_sequence(text).is_some() {
                chunks.push(std::mem::take(&mut self.buffer));
                return chunks;
            }
        }

        if starts_with_incomplete_utf8_char(&self.buffer) {
            tracing::trace!(bytes = ?self.buffer, "waiting for UTF-8 continuation bytes");
            return chunks;
        }

        if self.buffer.first() == Some(&ESC) && starts_with_incomplete_utf8_char(&self.buffer[1..])
        {
            tracing::trace!(bytes = ?self.buffer, "waiting for escaped UTF-8 continuation bytes");
            return chunks;
        }

        tracing::debug!(bytes = ?self.buffer, "dropping incomplete raw input buffer after timeout");
        self.lone_escape_recently_flushed = false;
        self.buffer.clear();
        chunks
    }

    fn drain_available_chunks(&mut self) -> Vec<Vec<u8>> {
        let mut chunks = Vec::new();

        loop {
            if self.lone_escape_recently_flushed {
                if starts_with_incomplete_orphaned_sgr_mouse_tail(&self.buffer) {
                    break;
                }
                if discard_complete_orphaned_sgr_mouse_tail(&mut self.buffer) {
                    self.lone_escape_recently_flushed = false;
                    continue;
                }
                self.lone_escape_recently_flushed = false;
            }

            if let Some(family) = self.discard_until {
                let Some(terminator_len) =
                    control_string_terminator_for_family(&self.buffer, family)
                else {
                    break;
                };
                self.buffer.drain(..terminator_len);
                self.discard_until = None;
                self.discarded_tail_bytes = 0;
                continue;
            }

            let Some((event, consumed)) = extract_one_event(&self.buffer) else {
                break;
            };
            if matches!(event, RawInputEvent::HostDefaultColor { .. }) {
                self.host_color_replies_awaited = self.host_color_replies_awaited.saturating_sub(1);
            } else if self.host_color_scheme_change_tracking
                && matches!(event, RawInputEvent::HostColorSchemeChanged(_))
            {
                self.host_color_query_sent();
            }
            self.held_pending_color_esc = false;
            chunks.push(self.buffer[..consumed].to_vec());
            self.buffer.drain(..consumed);
        }

        chunks
    }
}

const MAX_DISCARDED_CONTROL_TAIL_BYTES: usize = 128;

fn plausible_control_string_tail(family: ControlStringFamily, buffer: &[u8]) -> bool {
    match family {
        ControlStringFamily::Osc => buffer.iter().all(|byte| {
            byte.is_ascii_digit()
                || matches!(
                    *byte,
                    b';' | b':'
                        | b'/'
                        | b'#'
                        | b'?'
                        | b'.'
                        | b'_'
                        | b'-'
                        | b'+'
                        | b'r'
                        | b'g'
                        | b'b'
                        | b'R'
                        | b'G'
                        | b'B'
                        | ESC
                )
        }),
        ControlStringFamily::StTerminated => buffer.last() == Some(&ESC),
        ControlStringFamily::HostColorSchemeCsi => buffer
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(*byte, b';' | b'?' | b'n')),
        ControlStringFamily::OrphanedSgrMouseTail => buffer
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(*byte, b';' | b'M' | b'm')),
    }
}

pub(crate) fn events_require_host_surface_redraw(
    events: &[RawInputEvent],
    redraw_on_focus_gained: bool,
) -> bool {
    redraw_on_focus_gained
        && events
            .iter()
            .any(|event| matches!(event, RawInputEvent::OuterFocusGained))
}

#[cfg(any(not(windows), test))]
pub(crate) fn events_require_host_terminal_theme_query(events: &[RawInputEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RawInputEvent::HostColorSchemeChanged(_)))
}

pub fn spawn_input_reader() -> mpsc::Receiver<RawInputEvent> {
    let (tx, rx) = mpsc::channel(256);

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut scratch = [0u8; 1024];
        let mut framer = RawInputFramer::default();
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();

        loop {
            match reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => {
                    send_raw_input_events(framer.push(&scratch[..n]), &tx);

                    if stdin_read_ready(&reader, RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS) == Some(false) {
                        let had_pending = framer.has_pending_input();
                        let events = framer.flush_timeout();
                        let held_escape = had_pending && events.is_empty();
                        send_raw_input_events(events, &tx);
                        if held_escape
                            && stdin_read_ready(&reader, RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS)
                                == Some(false)
                        {
                            send_raw_input_events(framer.flush_timeout(), &tx);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    rx
}

fn send_raw_input_events(events: Vec<RawInputEvent>, tx: &mpsc::Sender<RawInputEvent>) {
    for event in events {
        let _ = tx.blocking_send(event);
    }
}

#[cfg(test)]
fn drain_buffer(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>) {
    for bytes in drain_complete_input_bytes(buffer) {
        let Some((event, _consumed)) = extract_one_event(&bytes) else {
            continue;
        };
        tracing::debug!(raw_bytes = ?bytes, event = ?event, "raw input event parsed");
        let _ = tx.blocking_send(event);
    }
}

#[cfg(test)]
pub(crate) fn drain_complete_input_bytes(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();

    while let Some((_event, consumed)) = extract_one_event(buffer) {
        chunks.push(buffer[..consumed].to_vec());
        buffer.drain(..consumed);
    }

    chunks
}

#[cfg(test)]
fn flush_incomplete_buffer(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>) {
    if let Some(bytes) = flush_incomplete_input_bytes(buffer) {
        if bytes.as_slice() == [ESC] {
            let _ = tx.blocking_send(RawInputEvent::Key(TerminalKey::new(
                crossterm::event::KeyCode::Esc,
                KeyModifiers::empty(),
            )));
            return;
        }

        let Some((event, _consumed)) = extract_one_event(&bytes) else {
            return;
        };
        let _ = tx.blocking_send(event);
    }
}

#[cfg(test)]
pub(crate) fn flush_incomplete_input_bytes(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut framer = RawInputByteFramer {
        buffer: std::mem::take(buffer),
        ..Default::default()
    };
    let mut chunks = framer.flush_timeout();
    *buffer = framer.buffer;
    chunks.pop()
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    #[cfg(unix)]
    {
        let fd = _reader.as_raw_fd();
        poll_read_ready(fd, _timeout_ms)
    }
}

#[cfg(not(unix))]
fn stdin_read_ready<R>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    None
}

#[cfg(unix)]
fn poll_read_ready(fd: i32, timeout_ms: i32) -> Option<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }

    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    }

    const POLLIN: i16 = 0x0001;

    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };

    let result = unsafe { poll(&mut pfd as *mut PollFd, 1, timeout_ms) };
    if result < 0 {
        None
    } else {
        Some(result > 0)
    }
}

fn extract_one_event(buffer: &[u8]) -> Option<(RawInputEvent, usize)> {
    if buffer.is_empty() {
        return None;
    }

    if buffer.starts_with(BRACKETED_PASTE_START) {
        let end = find_subsequence(buffer, BRACKETED_PASTE_END)?;
        let content = std::str::from_utf8(&buffer[BRACKETED_PASTE_START.len()..end]).ok()?;
        return Some((
            RawInputEvent::Paste(content.to_string()),
            end + BRACKETED_PASTE_END.len(),
        ));
    }

    if buffer[0] == ESC {
        let seq_len = complete_escape_sequence_len(buffer)?;
        let seq = std::str::from_utf8(&buffer[..seq_len]).ok()?;

        if let Some((kind, color)) = parse_default_color_response(seq) {
            return Some((RawInputEvent::HostDefaultColor { kind, color }, seq_len));
        }

        match seq {
            "\x1b[I" => return Some((RawInputEvent::OuterFocusGained, seq_len)),
            "\x1b[O" => return Some((RawInputEvent::OuterFocusLost, seq_len)),
            _ => {}
        }

        if let Some(appearance) = parse_host_color_scheme_report(&buffer[..seq_len]) {
            return Some((RawInputEvent::HostColorSchemeChanged(appearance), seq_len));
        }

        if let Some(mouse) = parse_sgr_mouse(seq) {
            return Some((RawInputEvent::Mouse(mouse), seq_len));
        }

        if let Some(key) = parse_terminal_key_sequence(seq) {
            return Some((RawInputEvent::Key(key), seq_len));
        }

        tracing::debug!(sequence = ?seq, "dropping unsupported escape sequence");
        return Some((RawInputEvent::Unsupported, seq_len));
    }

    let consumed = first_complete_utf8_char_len(buffer)?;
    let text = std::str::from_utf8(&buffer[..consumed]).ok()?;
    let key = parse_terminal_key_sequence(text)?;
    Some((RawInputEvent::Key(key), consumed))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlStringFamily {
    Osc,
    StTerminated,
    HostColorSchemeCsi,
    OrphanedSgrMouseTail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlString {
    Complete {
        len: usize,
        family: ControlStringFamily,
    },
    Incomplete {
        family: ControlStringFamily,
    },
}

fn parse_host_color_scheme_report(buffer: &[u8]) -> Option<HostAppearance> {
    match buffer {
        GHOSTTY_COLOR_SCHEME_DARK_REPORT => Some(HostAppearance::Dark),
        GHOSTTY_COLOR_SCHEME_LIGHT_REPORT => Some(HostAppearance::Light),
        _ => None,
    }
}

fn starts_with_incomplete_default_color_response(buffer: &[u8]) -> bool {
    matches!(
        control_string(buffer),
        Some(ControlString::Incomplete {
            family: ControlStringFamily::Osc
        })
    ) && matches!(buffer.get(..5), Some(b"\x1b]10;" | b"\x1b]11;"))
}

fn starts_with_incomplete_host_color_scheme_report(buffer: &[u8]) -> bool {
    buffer.starts_with(b"\x1b[?")
        && (GHOSTTY_COLOR_SCHEME_DARK_REPORT.starts_with(buffer)
            || GHOSTTY_COLOR_SCHEME_LIGHT_REPORT.starts_with(buffer))
        && buffer.len() < GHOSTTY_COLOR_SCHEME_DARK_REPORT.len()
}

fn control_string(buffer: &[u8]) -> Option<ControlString> {
    let family = match buffer.get(..2)? {
        b"\x1b]" => ControlStringFamily::Osc,
        b"\x1bP" | b"\x1b_" | b"\x1b^" | b"\x1bX" => ControlStringFamily::StTerminated,
        _ => return None,
    };

    Some(match control_string_terminator_for_family(buffer, family) {
        Some(len) => ControlString::Complete { len, family },
        None => ControlString::Incomplete { family },
    })
}

fn first_complete_utf8_char_len(buffer: &[u8]) -> Option<usize> {
    let width = utf8_char_width(*buffer.first()?)?;

    if buffer.len() < width {
        return None;
    }

    std::str::from_utf8(&buffer[..width]).ok()?;
    Some(width)
}

fn starts_with_incomplete_utf8_char(buffer: &[u8]) -> bool {
    match std::str::from_utf8(buffer) {
        Ok(_) => false,
        Err(err) => err.valid_up_to() == 0 && err.error_len().is_none(),
    }
}

fn utf8_char_width(first: u8) -> Option<usize> {
    if first < 0x80 {
        Some(1)
    } else if first & 0b1110_0000 == 0b1100_0000 {
        Some(2)
    } else if first & 0b1111_0000 == 0b1110_0000 {
        Some(3)
    } else if first & 0b1111_1000 == 0b1111_0000 {
        Some(4)
    } else {
        None
    }
}

fn complete_escape_sequence_len(buffer: &[u8]) -> Option<usize> {
    if buffer.len() == 1 {
        return None;
    }

    if buffer.starts_with(b"\x1b\x1b") {
        return complete_escape_sequence_len(&buffer[1..]).map(|len| len + 1);
    }

    if buffer.starts_with(b"\x1b[") {
        if buffer.starts_with(b"\x1b[<") {
            return find_csi_final(buffer, b"Mm");
        }
        return find_csi_final(
            buffer,
            b"@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
        );
    }

    if let Some(control) = control_string(buffer) {
        return match control {
            ControlString::Complete { len, .. } => Some(len),
            ControlString::Incomplete { .. } => None,
        };
    }

    if buffer.starts_with(b"\x1bO") {
        return (buffer.len() >= 3).then_some(3);
    }

    let escaped_char_width = utf8_char_width(buffer[1])?;
    if buffer.len() < 1 + escaped_char_width {
        return None;
    }
    std::str::from_utf8(&buffer[1..1 + escaped_char_width]).ok()?;
    Some(1 + escaped_char_width)
}

fn starts_with_incomplete_orphaned_sgr_mouse_tail(buffer: &[u8]) -> bool {
    if buffer.len() > MAX_ORPHANED_SGR_MOUSE_TAIL_BYTES {
        return false;
    }
    buffer.len() < 3 && b"[<".starts_with(buffer)
        || buffer.starts_with(b"[<")
            && buffer[2..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || *byte == b';')
}

fn discard_complete_orphaned_sgr_mouse_tail(buffer: &mut Vec<u8>) -> bool {
    let Some(terminator_len) =
        control_string_terminator_for_family(buffer, ControlStringFamily::OrphanedSgrMouseTail)
    else {
        return false;
    };
    if terminator_len > MAX_ORPHANED_SGR_MOUSE_TAIL_BYTES {
        return false;
    }
    let mut sequence = Vec::with_capacity(terminator_len + 1);
    sequence.push(ESC);
    sequence.extend_from_slice(&buffer[..terminator_len]);
    let Ok(sequence) = std::str::from_utf8(&sequence) else {
        return false;
    };
    if parse_sgr_mouse(sequence).is_none() {
        return false;
    }
    buffer.drain(..terminator_len);
    true
}

fn discard_or_buffer_orphaned_sgr_mouse_tail(
    buffer: &mut Vec<u8>,
    discard_until: &mut Option<ControlStringFamily>,
    discarded_tail_bytes: &mut usize,
) {
    if !discard_complete_orphaned_sgr_mouse_tail(buffer) {
        *discard_until = Some(ControlStringFamily::OrphanedSgrMouseTail);
        *discarded_tail_bytes = 0;
        buffer.clear();
    }
}

fn osc_string_terminator(buffer: &[u8]) -> Option<usize> {
    let st = find_subsequence(buffer, b"\x1b\\").map(|idx| idx + 2);
    let bel = buffer
        .iter()
        .position(|byte| *byte == b'\x07')
        .map(|idx| idx + 1);

    match (st, bel) {
        (Some(st), Some(bel)) => Some(st.min(bel)),
        (Some(st), None) => Some(st),
        (None, Some(bel)) => Some(bel),
        (None, None) => None,
    }
}

fn st_string_terminator(buffer: &[u8]) -> Option<usize> {
    find_subsequence(buffer, b"\x1b\\").map(|idx| idx + 2)
}

fn control_string_terminator_for_family(
    buffer: &[u8],
    family: ControlStringFamily,
) -> Option<usize> {
    match family {
        ControlStringFamily::Osc => osc_string_terminator(buffer),
        ControlStringFamily::StTerminated => st_string_terminator(buffer),
        ControlStringFamily::HostColorSchemeCsi => buffer
            .iter()
            .position(|byte| *byte == b'n')
            .map(|idx| idx + 1),
        ControlStringFamily::OrphanedSgrMouseTail => buffer
            .iter()
            .position(|byte| matches!(*byte, b'M' | b'm'))
            .map(|idx| idx + 1),
    }
}

fn find_csi_final(buffer: &[u8], finals: &[u8]) -> Option<usize> {
    for (idx, byte) in buffer.iter().enumerate().skip(2) {
        if finals.contains(byte) {
            return Some(idx + 1);
        }
    }
    None
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_sgr_mouse(sequence: &str) -> Option<MouseEvent> {
    let body = sequence.strip_prefix("\x1b[<")?;
    let final_char = body.chars().last()?;
    if final_char != 'M' && final_char != 'm' {
        return None;
    }

    let payload = &body[..body.len() - 1];
    let mut parts = payload.split(';');
    let cb = parts.next()?.parse::<u8>().ok()?;
    let column = parts.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = parts.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let (kind, modifiers) = parse_mouse_cb(cb)?;

    let kind = if final_char == 'm' {
        match kind {
            MouseEventKind::Down(button) => MouseEventKind::Up(button),
            other => other,
        }
    } else {
        kind
    };

    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

fn parse_mouse_cb(cb: u8) -> Option<(MouseEventKind, KeyModifiers)> {
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let dragging = cb & 0b0010_0000 == 0b0010_0000;

    let kind = match (button_number, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };

    let mut modifiers = KeyModifiers::empty();
    if cb & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cb & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if cb & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }

    Some((kind, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind};

    fn assert_raw_key(event: RawInputEvent, code: KeyCode, modifiers: KeyModifiers) {
        let RawInputEvent::Key(key) = event else {
            panic!("expected key");
        };
        assert_eq!(key.code, code);
        assert_eq!(key.modifiers, modifiers);
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

    fn collect_events(rx: &mut mpsc::Receiver<RawInputEvent>) -> Vec<RawInputEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn drain_chunk(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>, chunk: &[u8]) {
        buffer.extend_from_slice(chunk);
        drain_buffer(buffer, tx);
    }

    #[test]
    fn parses_kitty_shift_letter_release() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[108:76;2:3u").unwrap()
        else {
            panic!("expected key");
        };
        assert_eq!(consumed, 13);
        assert_eq!(key.code, KeyCode::Char('l'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.kind, KeyEventKind::Release);
        assert_eq!(key.shifted_codepoint, Some('L' as u32));
    }

    #[test]
    fn parses_bracketed_paste() {
        let (RawInputEvent::Paste(text), consumed) =
            extract_one_event(b"\x1b[200~hello\x1b[201~rest").unwrap()
        else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello");
        assert_eq!(consumed, 17);
    }

    #[test]
    fn parses_sgr_mouse() {
        let (RawInputEvent::Mouse(mouse), consumed) = extract_one_event(b"\x1b[<0;20;10M").unwrap()
        else {
            panic!("expected mouse");
        };
        assert_eq!(consumed, 11);
        assert_eq!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(mouse.column, 19);
        assert_eq!(mouse.row, 9);
        assert_eq!(mouse.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn parses_sgr_mouse_observable_modifiers() {
        let cases = [
            (b"\x1b[<8;20;10M".as_slice(), KeyModifiers::ALT),
            (b"\x1b[<16;20;10M".as_slice(), KeyModifiers::CONTROL),
            (
                b"\x1b[<24;20;10M".as_slice(),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            ),
        ];

        for (input, expected) in cases {
            let (RawInputEvent::Mouse(mouse), _) = extract_one_event(input).unwrap() else {
                panic!("expected mouse");
            };
            assert_eq!(mouse.modifiers, expected);
            assert!(!mouse.modifiers.contains(KeyModifiers::SUPER));
        }
    }

    #[test]
    fn parses_host_default_color_response_with_st() {
        let (RawInputEvent::HostDefaultColor { kind, color }, consumed) =
            extract_one_event(b"\x1b]10;rgb:cccc/dddd/eeee\x1b\\").unwrap()
        else {
            panic!("expected host color response");
        };
        assert_eq!(consumed, 25);
        assert_eq!(kind, DefaultColorKind::Foreground);
        assert_eq!(
            color,
            RgbColor {
                r: 0xcc,
                g: 0xdd,
                b: 0xee
            }
        );
    }

    #[test]
    fn parses_host_default_color_response_with_bel() {
        let (RawInputEvent::HostDefaultColor { kind, color }, consumed) =
            extract_one_event(b"\x1b]11;#112233\x07").unwrap()
        else {
            panic!("expected host color response");
        };
        assert_eq!(consumed, 13);
        assert_eq!(kind, DefaultColorKind::Background);
        assert_eq!(
            color,
            RgbColor {
                r: 0x11,
                g: 0x22,
                b: 0x33
            }
        );
    }

    #[test]
    fn parses_legacy_up_arrow() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[A").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 3);
        assert_eq!(key.code, KeyCode::Up);
    }

    #[test]
    fn parses_outer_focus_events() {
        let (event, consumed) = extract_one_event(b"\x1b[I").unwrap();
        assert_eq!(consumed, 3);
        assert!(matches!(event, RawInputEvent::OuterFocusGained));

        let (event, consumed) = extract_one_event(b"\x1b[O").unwrap();
        assert_eq!(consumed, 3);
        assert!(matches!(event, RawInputEvent::OuterFocusLost));
    }

    #[test]
    fn outer_focus_gained_requests_host_surface_redraw() {
        let events = parse_raw_input_bytes_sync(b"\x1b[I");
        assert!(events_require_host_surface_redraw(&events, true));
        assert!(!events_require_host_surface_redraw(&events, false));

        let events = parse_raw_input_bytes_sync(b"\x1b[O");
        assert!(!events_require_host_surface_redraw(&events, true));
    }

    #[test]
    fn parses_ghostty_color_scheme_reports() {
        for bytes in [
            GHOSTTY_COLOR_SCHEME_DARK_REPORT,
            GHOSTTY_COLOR_SCHEME_LIGHT_REPORT,
        ] {
            let events = parse_raw_input_bytes_sync(bytes);
            assert_eq!(events.len(), 1, "bytes: {bytes:?}");
            assert!(matches!(
                events[0],
                RawInputEvent::HostColorSchemeChanged(HostAppearance::Dark | HostAppearance::Light)
            ));
            assert!(events_require_host_terminal_theme_query(&events));
        }
    }

    #[test]
    fn ghostty_color_scheme_report_parser_is_exact() {
        for bytes in [
            b"\x1b[?997;0n".as_slice(),
            b"\x1b[?997;3n".as_slice(),
            b"\x1b[?998;1n".as_slice(),
        ] {
            let events = parse_raw_input_bytes_sync(bytes);
            assert_eq!(events.len(), 1, "bytes: {bytes:?}");
            assert!(matches!(events[0], RawInputEvent::Unsupported));
            assert!(!events_require_host_terminal_theme_query(&events));
        }
    }

    #[test]
    fn raw_input_framer_reassembles_split_color_scheme_report() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b[?997;").is_empty());
        let events = framer.push(b"1n");

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            RawInputEvent::HostColorSchemeChanged(HostAppearance::Dark)
        ));
    }

    #[test]
    fn split_color_scheme_timeout_does_not_swallow_legacy_alt_bracket() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b[").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b[".to_vec()]);
    }

    #[test]
    fn raw_input_byte_framer_discards_timed_out_split_color_scheme_report_tail() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b[?997;").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert!(framer.push(b"1n").is_empty());
        assert_eq!(framer.push(b"a"), vec![b"a".to_vec()]);
        assert!(framer.flush_timeout().is_empty());
    }

    #[test]
    fn parses_xterm_alt_up_arrow() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[1;3A").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 6);
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_legacy_alt_backspace() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b\x7f").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 2);
        assert_eq!(key.code, KeyCode::Backspace);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_kitty_alt_backspace() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[127;3u").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::Backspace);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_enhanced_pageup_press() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[5;1:1~").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::PageUp);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, KeyEventKind::Press);
    }

    #[test]
    fn parses_enhanced_pagedown_release() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[6;1:3~").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::PageDown);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, KeyEventKind::Release);
    }

    #[test]
    fn raw_input_family_matrix_is_covered() {
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            (b"\x02", KeyCode::Char('b'), KeyModifiers::CONTROL),
            (b"\r", KeyCode::Enter, KeyModifiers::empty()),
            (b"\t", KeyCode::Tab, KeyModifiers::empty()),
            (b"\x7f", KeyCode::Backspace, KeyModifiers::empty()),
            (b"\x1b[A", KeyCode::Up, KeyModifiers::empty()),
            (b"\x1b[1;3A", KeyCode::Up, KeyModifiers::ALT),
            (b"\x1b\x7f", KeyCode::Backspace, KeyModifiers::ALT),
            (b"\x1b[127;3u", KeyCode::Backspace, KeyModifiers::ALT),
            (b"\x1b[57420;1u", KeyCode::Down, KeyModifiers::empty()),
            (b"\x1b[57423;1u", KeyCode::Home, KeyModifiers::empty()),
            (b"\x1bOq", KeyCode::Char('1'), KeyModifiers::empty()),
            (b"\x1b[14~", KeyCode::F(4), KeyModifiers::empty()),
            (b"\x1b[49:33;2:1u", KeyCode::Char('1'), KeyModifiers::SHIFT),
        ];

        for (bytes, code, modifiers) in cases {
            let (event, consumed) = extract_one_event(bytes).unwrap();
            assert_eq!(consumed, bytes.len());
            assert_raw_key(event, *code, *modifiers);
        }
    }

    #[test]
    fn raw_framer_waits_for_application_keypad_sequence_final_byte() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1bO").is_empty());
        let events = framer.push(b"q");

        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('1'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn unsupported_ss3_sequence_stays_unsupported() {
        let (event, consumed) = extract_one_event(b"\x1bOz").unwrap();

        assert_eq!(consumed, 3);
        assert!(matches!(event, RawInputEvent::Unsupported));
    }

    #[test]
    fn modified_rxvt_f_key_alias_stays_unsupported() {
        let (event, consumed) = extract_one_event(b"\x1b[14;3~").unwrap();

        assert_eq!(consumed, 7);
        assert!(matches!(event, RawInputEvent::Unsupported));
    }

    #[test]
    fn flushes_lone_escape_after_timeout() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut buffer = vec![ESC];
        flush_incomplete_buffer(&mut buffer, &tx);
        assert!(buffer.is_empty());
        let event = rx.try_recv().unwrap();
        let RawInputEvent::Key(key) = event else {
            panic!("expected key");
        };
        assert_eq!(key.code, KeyCode::Esc);
    }

    #[test]
    fn parses_raw_ctrl_b() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x02").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 1);
        assert_eq!(key.code, KeyCode::Char('b'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parses_raw_lf_as_ctrl_j() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\n").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 1);
        assert_eq!(key.code, KeyCode::Char('j'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    fn assert_fixture_extracts_whole_events(corpus: &str, macos_layout: bool) {
        for line in corpus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut columns: Vec<_> = line.split('\t').collect();
            if columns.len() == 5 {
                columns.push("");
            }

            if macos_layout {
                if columns.len() == 6 {
                    columns.push("");
                }
                assert_eq!(
                    columns.len(),
                    7,
                    "macOS fixture row must have 7 columns: {line}"
                );
                if columns[2].is_empty() {
                    continue;
                }
                let bytes = decode_hex(columns[2]);
                let (event, consumed) = extract_one_event(&bytes).unwrap();
                assert_eq!(
                    consumed,
                    bytes.len(),
                    "fixture should extract a whole event: {line}"
                );
                assert_raw_key(
                    event,
                    parse_fixture_key_code(columns[3]),
                    parse_fixture_modifiers(columns[4]),
                );
            } else {
                if columns.len() == 5 {
                    columns.push("");
                }
                let (bytes_hex, code, modifiers) = match columns.len() {
                    6 => {
                        if columns[1].chars().all(|ch| ch.is_ascii_hexdigit()) {
                            (columns[1], columns[2], columns[3])
                        } else {
                            (columns[2], columns[3], columns[4])
                        }
                    }
                    7 => (columns[2], columns[3], columns[4]),
                    _ => panic!("fixture row must have 6 or 7 columns: {line}"),
                };
                assert!(
                    bytes_hex.chars().all(|ch| ch.is_ascii_hexdigit()),
                    "non-hex fixture bytes: {bytes_hex} in {line}"
                );
                let bytes = decode_hex(bytes_hex);
                let (event, consumed) = extract_one_event(&bytes).unwrap();
                assert_eq!(
                    consumed,
                    bytes.len(),
                    "fixture should extract a whole event: {line}"
                );
                assert_raw_key(
                    event,
                    parse_fixture_key_code(code),
                    parse_fixture_modifiers(modifiers),
                );
            }
        }
    }

    #[test]
    fn raw_input_corpus_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/keyboard_protocol_corpus.tsv");
        assert_fixture_extracts_whole_events(corpus, false);
    }

    #[test]
    fn raw_input_macos_terminal_variants_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/macos_terminal_variants.tsv");
        assert_fixture_extracts_whole_events(corpus, true);
    }

    #[test]
    fn raw_input_linux_terminal_variants_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/linux_terminal_variants.tsv");
        assert_fixture_extracts_whole_events(corpus, false);
    }

    #[test]
    fn chunked_legacy_arrow_waits_for_completion() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"[A");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Up,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn lone_escape_is_buffered_until_timeout_flush() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        flush_incomplete_buffer(&mut buffer, &tx);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Esc,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn escape_followed_by_arrow_before_flush_does_not_emit_escape() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"[B");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Down,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn escape_followed_by_sgr_mouse_before_flush_does_not_emit_text() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        let events = framer.push(b"[<65;43;26M");

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 42,
                row: 25,
                ..
            })
        ));
    }

    #[test]
    fn sgr_mouse_tail_after_lone_escape_timeout_is_discarded() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        let timeout_events = framer.flush_timeout();
        assert_eq!(timeout_events.len(), 1);
        assert_raw_key(
            timeout_events.into_iter().next().unwrap(),
            KeyCode::Esc,
            KeyModifiers::empty(),
        );

        assert!(framer.push(b"[<65;43;26M").is_empty());
    }

    #[test]
    fn input_after_discarded_complete_sgr_mouse_tail_is_preserved() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout().len(), 1);
        let events = framer.push(b"[<65;43;26Mx");
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn invalid_orphaned_sgr_mouse_tail_after_escape_timeout_is_preserved() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout().len(), 1);

        let events = framer.push(b"[<x");

        assert_eq!(events.len(), 3);
        assert_raw_key(
            events.into_iter().last().unwrap(),
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn double_split_sgr_mouse_tail_after_lone_escape_timeout_is_discarded() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout().len(), 1);

        assert!(framer.push(b"[<65;4").is_empty());
        assert!(framer.flush_timeout().is_empty());
        let events = framer.push(b"3;26Mx");
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn escape_followed_by_alt_char_before_flush_becomes_alt_key() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"b");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('b'),
            KeyModifiers::ALT,
        );
    }

    #[test]
    fn chunked_kitty_sequence_waits_for_completion() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[49:33;2:");
        assert_eq!(buffer, b"\x1b[49:33;2:");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"1u");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('1'),
            KeyModifiers::SHIFT,
        );
    }

    #[test]
    fn chunked_bracketed_paste_waits_for_terminator() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[200~hello");
        assert_eq!(buffer, b"\x1b[200~hello");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"\x1b[201~");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        let RawInputEvent::Paste(text) = &events[0] else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello");
    }

    #[test]
    fn incomplete_bracketed_paste_is_not_flushed_on_timeout() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[200~hello\nworld");
        assert_eq!(buffer, b"\x1b[200~hello\nworld");
        assert!(collect_events(&mut rx).is_empty());

        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, b"\x1b[200~hello\nworld");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"\x1b[201~");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        let RawInputEvent::Paste(text) = &events[0] else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn complete_utf8_char_before_incomplete_char_is_drained() {
        let mut buffer = "你".as_bytes().to_vec();
        buffer.push("好".as_bytes()[0]);

        let chunks = drain_complete_input_bytes(&mut buffer);

        assert_eq!(chunks, vec!["你".as_bytes().to_vec()]);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);
    }

    #[test]
    fn incomplete_utf8_prefix_is_not_flushed_on_timeout() {
        let mut buffer = vec!["好".as_bytes()[0]];

        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);
    }

    #[test]
    fn invalid_utf8_lead_byte_is_flushed_instead_of_buffered_forever() {
        let mut buffer = vec![0xC0];

        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert!(buffer.is_empty());
    }

    #[test]
    fn complete_utf8_char_before_incomplete_char_survives_timeout_and_next_chunk() {
        let mut buffer = "你".as_bytes().to_vec();
        buffer.push("好".as_bytes()[0]);

        let chunks = drain_complete_input_bytes(&mut buffer);
        assert_eq!(chunks, vec!["你".as_bytes().to_vec()]);
        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);

        buffer.extend_from_slice(&"好".as_bytes()[1..]);
        let chunks = drain_complete_input_bytes(&mut buffer);
        assert_eq!(chunks, vec!["好".as_bytes().to_vec()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn alt_utf8_char_drains_as_one_event_before_following_input() {
        let events = parse_raw_input_bytes_sync("\x1béx".as_bytes());
        assert_eq!(events.len(), 2);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('é'),
            KeyModifiers::ALT,
        );
    }

    #[test]
    fn chunked_alt_utf8_waits_for_continuation_byte_after_escape() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();
        let bytes = "\x1bé".as_bytes();

        drain_chunk(&mut buffer, &tx, &bytes[..2]);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());
        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, &bytes[2..]);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('é'),
            KeyModifiers::ALT,
        );
    }

    #[test]
    fn chunked_utf8_waits_for_continuation_byte() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, "é".as_bytes().get(..1).unwrap());
        assert_eq!(buffer, vec![0xC3]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, "é".as_bytes().get(1..).unwrap());
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('é'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn chunked_cjk_utf8_waits_for_all_continuation_bytes() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();
        let bytes = "好".as_bytes();

        drain_chunk(&mut buffer, &tx, &bytes[..1]);
        assert_eq!(buffer, bytes[..1]);
        assert!(collect_events(&mut rx).is_empty());
        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, bytes[..1]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, &bytes[1..2]);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());
        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, &bytes[2..]);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('好'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn chunked_four_byte_utf8_waits_for_all_continuation_bytes() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();
        let bytes = "🙂".as_bytes();

        for split in 1..bytes.len() {
            drain_chunk(&mut buffer, &tx, &bytes[split - 1..split]);
            assert_eq!(buffer, bytes[..split]);
            assert!(collect_events(&mut rx).is_empty());
            flush_incomplete_buffer(&mut buffer, &tx);
            assert_eq!(buffer, bytes[..split]);
            assert!(collect_events(&mut rx).is_empty());
        }

        drain_chunk(&mut buffer, &tx, &bytes[bytes.len() - 1..]);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('🙂'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn long_multilingual_voice_like_burst_drains_without_truncation() {
        let text = "你好，今天我们测试一段比较长的语音输入。こんにちは。안녕하세요.🙂".repeat(128);
        assert!(
            text.len() > 4096,
            "test input should exceed the client read buffer"
        );
        let mut buffer = text.as_bytes().to_vec();

        let chunks = drain_complete_input_bytes(&mut buffer);
        let rebuilt: Vec<u8> = chunks.into_iter().flatten().collect();

        assert!(buffer.is_empty());
        assert_eq!(rebuilt, text.as_bytes());
    }

    #[test]
    fn long_multilingual_burst_survives_one_byte_chunks_and_timeouts() {
        let text = "中文かなカナ한글🙂，。".repeat(64);
        let mut buffer = Vec::new();
        let mut rebuilt = Vec::new();

        for byte in text.as_bytes() {
            buffer.push(*byte);
            for chunk in drain_complete_input_bytes(&mut buffer) {
                rebuilt.extend(chunk);
            }
            if !buffer.is_empty() {
                assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
            }
        }

        for chunk in drain_complete_input_bytes(&mut buffer) {
            rebuilt.extend(chunk);
        }

        assert!(buffer.is_empty());
        assert_eq!(rebuilt, text.as_bytes());
    }

    #[test]
    fn parse_with_ranges_tracks_byte_offsets() {
        use super::parse_raw_input_bytes_with_ranges;

        // Input: Up arrow (3 bytes) + 'a' (1 byte) + Down arrow (3 bytes)
        let input = b"\x1b[Aa\x1b[B".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 3, "should parse three events");

        // Up arrow: \x1b[A at offset 0, length 3
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 3);
        assert!(matches!(
            &ranges[0].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Up
        ));

        // 'a' at offset 3, length 1
        assert_eq!(ranges[1].start, 3);
        assert_eq!(ranges[1].len, 1);
        assert!(matches!(
            &ranges[1].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Char('a')
        ));

        // Down arrow: \x1b[B at offset 4, length 3
        assert_eq!(ranges[2].start, 4);
        assert_eq!(ranges[2].len, 3);
        assert!(matches!(
            &ranges[2].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Down
        ));

        // Verify the raw bytes for each event slice correctly.
        assert_eq!(
            &input[ranges[0].start..ranges[0].start + ranges[0].len],
            b"\x1b[A"
        );
        assert_eq!(
            &input[ranges[1].start..ranges[1].start + ranges[1].len],
            b"a"
        );
        assert_eq!(
            &input[ranges[2].start..ranges[2].start + ranges[2].len],
            b"\x1b[B"
        );
    }

    #[test]
    fn parse_with_ranges_handles_single_event() {
        use super::parse_raw_input_bytes_with_ranges;

        let input = b"a".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 1);
    }

    #[test]
    fn parse_with_ranges_handles_mouse_event() {
        use super::parse_raw_input_bytes_with_ranges;

        let input = b"\x1b[<0;20;10M".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, input.len());
        assert!(matches!(&ranges[0].event, RawInputEvent::Mouse(_)));
    }

    #[test]
    fn parses_ghostty_default_background_response() {
        let events = parse_raw_input_bytes_sync(b"\x1b]11;rgb:2828/2a2a/3636\x07");

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Background,
                color: RgbColor {
                    r: 0x28,
                    g: 0x2a,
                    b: 0x36
                }
            }
        ));
    }

    #[test]
    fn drain_complete_input_bytes_keeps_split_default_background_response_buffered() {
        let mut buffer = b"\x1b]11;rgb:2828".to_vec();

        let chunks = drain_complete_input_bytes(&mut buffer);

        assert!(chunks.is_empty());
        assert_eq!(buffer, b"\x1b]11;rgb:2828");
    }

    #[test]
    fn flush_incomplete_input_bytes_keeps_split_default_background_response_buffered() {
        let mut buffer = b"\x1b]11;rgb:2828".to_vec();

        let flushed = flush_incomplete_input_bytes(&mut buffer);

        assert!(flushed.is_none());
        assert_eq!(buffer, b"\x1b]11;rgb:2828");
    }

    #[test]
    fn flush_incomplete_input_bytes_keeps_default_background_response_split_after_command() {
        let mut buffer = b"\x1b]11;".to_vec();

        let flushed = flush_incomplete_input_bytes(&mut buffer);

        assert!(flushed.is_none());
        assert_eq!(buffer, b"\x1b]11;");
    }

    #[test]
    fn flush_incomplete_input_bytes_keeps_default_background_response_split_inside_st() {
        let mut buffer = b"\x1b]11;rgb:2828/2a2a/3636\x1b".to_vec();

        let flushed = flush_incomplete_input_bytes(&mut buffer);

        assert!(flushed.is_none());
        assert_eq!(buffer, b"\x1b]11;rgb:2828/2a2a/3636\x1b");
    }

    #[test]
    fn drain_complete_input_bytes_keeps_bare_osc_introducer_buffered() {
        let mut buffer = b"\x1b]".to_vec();

        let chunks = drain_complete_input_bytes(&mut buffer);

        assert!(chunks.is_empty());
        assert_eq!(buffer, b"\x1b]");
    }

    #[test]
    fn flush_incomplete_input_bytes_drops_bare_osc_introducer_after_timeout() {
        let mut buffer = b"\x1b]".to_vec();

        let flushed = flush_incomplete_input_bytes(&mut buffer);

        assert!(flushed.is_none());
        assert!(buffer.is_empty());
    }

    #[test]
    fn flush_incomplete_input_bytes_drops_string_introducers_after_timeout() {
        for bytes in [
            b"\x1b]".as_slice(),
            b"\x1bP".as_slice(),
            b"\x1b_".as_slice(),
            b"\x1b^".as_slice(),
            b"\x1bX".as_slice(),
        ] {
            let mut buffer = bytes.to_vec();

            let flushed = flush_incomplete_input_bytes(&mut buffer);

            assert!(flushed.is_none(), "flushed {bytes:?}");
            assert!(buffer.is_empty(), "kept {bytes:?}");
        }
    }

    #[test]
    fn raw_input_framer_reassembles_split_default_background_response() {
        let mut framer = RawInputFramer::default();

        assert!(framer.push(b"\x1b]").is_empty());
        let events = framer.push(b"11;#123456\x07");

        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Background,
                color: RgbColor {
                    r: 0x12,
                    g: 0x34,
                    b: 0x56,
                }
            }
        ));
    }

    #[test]
    fn raw_input_byte_framer_discards_split_control_string_after_timeout() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b]").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert!(framer.push(b"11;#123456\x07").is_empty());
        assert_eq!(framer.push(b"a"), vec![b"a".to_vec()]);
    }

    #[test]
    fn raw_input_byte_framer_keeps_discarding_tail_across_timeout() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b]").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert!(framer.push(b"1").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert!(framer.push(b"1;#123456\x07").is_empty());
        assert_eq!(framer.push(b"a"), vec![b"a".to_vec()]);
    }

    #[test]
    fn raw_input_byte_framer_releases_discard_on_implausible_tail() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b]").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert!(framer.push(b"a").is_empty());
        assert!(framer.flush_timeout().is_empty());
        assert_eq!(framer.push(b"b"), vec![b"b".to_vec()]);
    }

    #[test]
    fn parse_raw_input_bytes_sync_does_not_parse_incomplete_strings_as_alt_keys() {
        for bytes in [
            b"\x1b]".as_slice(),
            b"\x1bP".as_slice(),
            b"\x1b_".as_slice(),
            b"\x1b^".as_slice(),
            b"\x1bX".as_slice(),
        ] {
            let events = parse_raw_input_bytes_sync(bytes);

            assert!(events.is_empty(), "parsed {bytes:?} as {events:?}");
        }
    }

    #[test]
    fn non_osc_control_strings_ignore_bel_and_complete_at_st() {
        let bytes = b"\x1bPabc\x07def\x1b\\x";

        let (event, consumed) = extract_one_event(bytes).unwrap();

        assert!(matches!(event, RawInputEvent::Unsupported));
        assert_eq!(consumed, b"\x1bPabc\x07def\x1b\\".len());
    }

    #[test]
    fn non_osc_default_color_text_remains_key_input() {
        let events = parse_raw_input_bytes_sync(b"11;rgb:2828/2a2a/3636\x07");

        assert_eq!(events.len(), 22);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('1'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn flush_incomplete_input_bytes_does_not_hold_non_osc_default_color_text() {
        let mut framer = RawInputByteFramer::default();

        let chunks = framer.push(b"11;rgb:2828");

        assert_eq!(chunks.len(), 11);
        assert!(framer.flush_timeout().is_empty());
    }

    #[test]
    fn holds_lone_escape_and_stitches_split_host_color_reply() {
        let mut framer = RawInputByteFramer::default();
        framer.host_color_query_sent();

        // The reply is split right at its ESC introducer.
        assert!(framer.push(b"\x1b").is_empty());
        // The idle flush must not release the ESC as an Escape key while a host
        // color reply is still outstanding.
        assert!(framer.flush_timeout().is_empty());

        // The rest of the OSC 11 reply arrives and stitches back together
        // instead of leaking its payload into the focused pane.
        let chunks = framer.push(b"]11;rgb:2424/2727/3a3a\x1b\\");
        assert_eq!(chunks.len(), 1);
        let (event, _) = extract_one_event(&chunks[0]).unwrap();
        assert!(matches!(
            event,
            RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Background,
                ..
            }
        ));
    }

    #[test]
    fn default_byte_framer_does_not_rearm_after_color_scheme_report() {
        let mut framer = RawInputByteFramer::default();

        assert_eq!(
            framer.push(GHOSTTY_COLOR_SCHEME_DARK_REPORT),
            vec![GHOSTTY_COLOR_SCHEME_DARK_REPORT.to_vec()]
        );
        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }

    #[test]
    fn opt_in_does_not_delay_plain_escape_without_color_scheme_report() {
        let mut framer = RawInputByteFramer::default();
        framer.enable_host_color_scheme_change_tracking();

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }

    #[test]
    fn opted_in_byte_framer_rearms_after_color_scheme_report() {
        let mut framer = RawInputByteFramer::default();
        framer.enable_host_color_scheme_change_tracking();

        assert_eq!(
            framer.push(GHOSTTY_COLOR_SCHEME_DARK_REPORT),
            vec![GHOSTTY_COLOR_SCHEME_DARK_REPORT.to_vec()]
        );

        assert!(framer.push(b"\x1b").is_empty());
        assert!(framer.flush_timeout().is_empty());
        let chunks = framer.push(b"]10;#abcdef\x07");
        assert_eq!(chunks.len(), 1);
        let (event, _) = extract_one_event(&chunks[0]).unwrap();
        assert!(matches!(
            event,
            RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Foreground,
                color: RgbColor {
                    r: 0xab,
                    g: 0xcd,
                    b: 0xef
                }
            }
        ));

        assert!(framer.push(b"\x1b").is_empty());
        assert!(framer.flush_timeout().is_empty());
        let chunks = framer.push(b"]11;#123456\x07");
        assert_eq!(chunks.len(), 1);
        let (event, _) = extract_one_event(&chunks[0]).unwrap();
        assert!(matches!(
            event,
            RawInputEvent::HostDefaultColor {
                kind: DefaultColorKind::Background,
                color: RgbColor {
                    r: 0x12,
                    g: 0x34,
                    b: 0x56
                }
            }
        ));

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }

    #[test]
    fn flushes_lone_escape_when_not_awaiting_host_color_reply() {
        let mut framer = RawInputByteFramer::default();

        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }

    #[test]
    fn gives_up_holding_lone_escape_after_one_idle_flush() {
        let mut framer = RawInputByteFramer::default();
        framer.host_color_query_sent();

        assert!(framer.push(b"\x1b").is_empty());
        // First idle flush holds the escape.
        assert!(framer.flush_timeout().is_empty());
        // No continuation arrived; the second idle flush releases it as Escape.
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }

    #[test]
    fn stops_holding_lone_escape_after_host_color_reply_completes() {
        let mut framer = RawInputByteFramer::default();
        framer.host_color_query_sent();

        let chunks =
            framer.push(b"\x1b]10;rgb:6565/7b7b/8383\x1b\\\x1b]11;rgb:2424/2727/3a3a\x1b\\");
        assert_eq!(chunks.len(), 2);

        // Window closed: a later lone Escape flushes immediately.
        assert!(framer.push(b"\x1b").is_empty());
        assert_eq!(framer.flush_timeout(), vec![b"\x1b".to_vec()]);
    }
}
