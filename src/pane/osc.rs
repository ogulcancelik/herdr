use std::borrow::Cow;
use std::path::PathBuf;

use tracing::info;

use crate::layout::PaneId;

use super::terminal::GhosttyPaneCore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorQuery {
    Foreground,
    Background,
    Cursor,
}

impl DefaultColorQuery {
    pub(super) fn osc_number(self) -> u8 {
        match self {
            Self::Foreground => 10,
            Self::Background => 11,
            Self::Cursor => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorEvent {
    Query(DefaultColorQuery),
    Set(DefaultColorQuery),
    Reset(DefaultColorQuery),
    PaletteQuery(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DefaultColorTrackedEvent {
    pub(super) end_offset: usize,
    pub(super) event: DefaultColorEvent,
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorOscTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DefaultColorOscTrackerState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
    IgnoreString,
    IgnoreStringEscape,
    OversizedOsc,
    OversizedOscEscape,
}

fn is_ignored_string_intro(byte: u8) -> bool {
    matches!(byte, b'P' | b'_' | b'^' | b'X')
}

impl DefaultColorOscTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let mut saw_default_color_set = false;

        for &byte in bytes {
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }

        saw_default_color_set
    }
}

fn is_default_color_set_osc(body: &[u8]) -> bool {
    matches!(
        parse_default_color_event(body),
        Some(DefaultColorEvent::Set(_))
    )
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorEventTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
    pending: Vec<DefaultColorTrackedEvent>,
}

impl DefaultColorEventTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }
    }

    fn finalize(&mut self, end_offset: usize) {
        if let Some(event) = parse_default_color_event(&self.body) {
            self.pending
                .push(DefaultColorTrackedEvent { end_offset, event });
        }
        self.body.clear();
    }

    pub(super) fn drain_pending(&mut self) -> Vec<DefaultColorTrackedEvent> {
        std::mem::take(&mut self.pending)
    }
}

fn parse_default_color_event(body: &[u8]) -> Option<DefaultColorEvent> {
    match body {
        b"10;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Foreground)),
        b"11;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Background)),
        b"12;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Cursor)),
        b"110" | b"110;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Foreground)),
        b"111" | b"111;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Background)),
        _ => parse_palette_color_query(body).or_else(|| parse_default_color_set_event(body)),
    }
}

fn parse_palette_color_query(body: &[u8]) -> Option<DefaultColorEvent> {
    let index = body.strip_prefix(b"4;")?.strip_suffix(b";?")?;
    if index.is_empty() || index.len() > 3 || !index.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: u16 = 0;
    for &digit in index {
        value = value * 10 + u16::from(digit - b'0');
    }
    u8::try_from(value)
        .ok()
        .map(DefaultColorEvent::PaletteQuery)
}

fn parse_default_color_set_event(body: &[u8]) -> Option<DefaultColorEvent> {
    let separator = body.iter().position(|byte| *byte == b';')?;
    let query = match &body[..separator] {
        b"10" => DefaultColorQuery::Foreground,
        b"11" => DefaultColorQuery::Background,
        _ => return None,
    };
    let value = &body[separator + 1..];
    (!value.is_empty() && value != b"?").then_some(DefaultColorEvent::Set(query))
}

/// 256 KiB of base64 ≈ 192 KiB of text — enough for real source-file copies
/// while still bounding memory against stream garbage.
const OSC52_MAX_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Osc52ForwarderState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
}

/// Reconstructs OSC 52 clipboard-write sequences from raw PTY bytes so the
/// main loop can re-emit them. `libghostty-vt` drops `.clipboard_contents`,
/// so child clipboard writes never reach the host terminal unless we forward
/// them ourselves.
#[derive(Debug, Default)]
pub(super) struct Osc52Forwarder {
    state: Osc52ForwarderState,
    body: Vec<u8>,
    pending: Vec<Vec<u8>>,
}

impl Osc52Forwarder {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                Osc52ForwarderState::Ground => {
                    if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    }
                }
                Osc52ForwarderState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = Osc52ForwarderState::OscBody;
                    } else if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    } else {
                        self.state = Osc52ForwarderState::Ground;
                    }
                }
                Osc52ForwarderState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    }
                    0x1b => self.state = Osc52ForwarderState::OscEscape,
                    _ => self.body.push(byte),
                },
                Osc52ForwarderState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = Osc52ForwarderState::OscBody;
                    }
                }
            }

            if self.body.len() > OSC52_MAX_PAYLOAD_BYTES {
                self.body.clear();
                self.state = Osc52ForwarderState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some(content) = parse_osc52_clipboard_write(&self.body) {
            self.pending.push(content);
        }
        self.body.clear();
    }

    pub(super) fn drain_pending(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending)
    }
}

/// Reconstructs cwd-reporting OSC sequences from child output. Shell
/// integrations commonly use OSC 7 (`file://...`), while Windows Terminal
/// documents OSC 9;9 for the same practical purpose.
#[derive(Debug, Default)]
pub(super) struct CwdOscTracker {
    state: Osc52ForwarderState,
    body: Vec<u8>,
    pending: Vec<PathBuf>,
}

impl CwdOscTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                Osc52ForwarderState::Ground => {
                    if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    }
                }
                Osc52ForwarderState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = Osc52ForwarderState::OscBody;
                    } else if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    } else {
                        self.state = Osc52ForwarderState::Ground;
                    }
                }
                Osc52ForwarderState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    }
                    0x1b => self.state = Osc52ForwarderState::OscEscape,
                    _ => self.body.push(byte),
                },
                Osc52ForwarderState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = Osc52ForwarderState::OscBody;
                    }
                }
            }

            if self.body.len() > 4096 {
                self.body.clear();
                self.state = Osc52ForwarderState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some(cwd) = parse_cwd_osc(&self.body) {
            self.pending.push(cwd);
        }
        self.body.clear();
    }

    pub(super) fn drain_latest(&mut self) -> Option<PathBuf> {
        self.pending.drain(..).next_back()
    }
}

fn parse_cwd_osc(body: &[u8]) -> Option<PathBuf> {
    let body = std::str::from_utf8(body).ok()?;
    if let Some(uri) = body.strip_prefix("7;") {
        return parse_file_uri_cwd(uri);
    }
    if let Some(path) = body.strip_prefix("9;9;") {
        let path = path.trim().trim_matches('"');
        return (!path.is_empty()).then(|| PathBuf::from(path));
    }
    None
}

/// Maximum retained string length for agent OSC title and progress payloads.
/// Title text is untrusted model output; cap it to bound memory and log size.
const AGENT_OSC_MAX_CHARS: usize = 256;

/// Always-on tracker that retains the latest OSC 0/2 title and OSC 9 progress
/// payload emitted by the child process. Nothing here affects rendering; this
/// is pure passive capture for the detection engine (Stage C / Stage D).
///
/// - `latest_title` — last OSC 0 or OSC 2 payload, sanitized. An empty
///   payload (e.g. `\x1b]0;\x07`) clears the stored value.
/// - `latest_progress` — last OSC 9 payload (the part after `9;`), stored
///   as-is after sanitization. E.g. `"4;3;"` or `"4;0;"`.
#[derive(Debug, Default)]
pub(super) struct AgentOscStateTracker {
    state: Osc52ForwarderState,
    body: Vec<u8>,
    latest_title: Option<String>,
    terminal_title: Option<String>,
    latest_progress: Option<String>,
}

impl AgentOscStateTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                Osc52ForwarderState::Ground => {
                    if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    }
                }
                Osc52ForwarderState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = Osc52ForwarderState::OscBody;
                    } else if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    } else {
                        self.state = Osc52ForwarderState::Ground;
                    }
                }
                Osc52ForwarderState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    }
                    0x1b => self.state = Osc52ForwarderState::OscEscape,
                    _ => self.body.push(byte),
                },
                Osc52ForwarderState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = Osc52ForwarderState::OscBody;
                    }
                }
            }

            if self.body.len() > 4096 {
                self.body.clear();
                self.state = Osc52ForwarderState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some((command, payload)) = parse_agent_osc_body(&self.body) {
            match command {
                b"0" | b"2" => {
                    let title = if payload.is_empty() {
                        None
                    } else {
                        let title = sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS);
                        (!title.is_empty()).then_some(title)
                    };
                    self.latest_title.clone_from(&title);
                    self.terminal_title = title;
                }
                b"9" => {
                    self.latest_progress =
                        Some(sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS));
                }
                _ => {}
            }
        }
        self.body.clear();
    }

    pub(super) fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }

    #[cfg(unix)]
    pub(super) fn seed_terminal_title(&mut self, title: Option<String>) {
        self.terminal_title = title;
    }

    /// Returns the latest retained OSC title, or `""` if none has been seen or
    /// the last title was an empty clear.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_title(&self) -> &str {
        self.latest_title.as_deref().unwrap_or("")
    }

    /// Returns the latest retained OSC 9 progress payload, or `""` if none.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_progress(&self) -> &str {
        self.latest_progress.as_deref().unwrap_or("")
    }

    /// Drops the retained title and progress so a new foreground agent cannot
    /// inherit OSC evidence emitted by a previous process. The in-flight parse
    /// state is kept: a sequence spanning the agent change finalizes normally
    /// and is attributed to the new agent.
    pub(super) fn clear_retained(&mut self) {
        self.latest_title = None;
        self.latest_progress = None;
    }
}

/// Splits an OSC body at the first `;`, returning `(command, payload)`.
/// Returns `None` if there is no `;`.
fn parse_agent_osc_body(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = body.iter().position(|&b| b == b';')?;
    Some((&body[..sep], &body[sep + 1..]))
}

fn sanitize_agent_osc_string(payload: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(payload);
    let mut out = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(max_chars) {
        out.push(ch);
    }
    out
}

/// Reconstructs selected OSC sequences for local evidence capture while
/// debugging agent title/status behavior. This is intentionally passive:
/// nothing here affects terminal rendering or detection state.
#[derive(Debug)]
pub(super) struct OscDebugTracker {
    enabled: bool,
    state: Osc52ForwarderState,
    body: Vec<u8>,
    pending: Vec<OscDebugEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OscDebugEvent {
    pub(super) command: String,
    pub(super) payload: String,
}

impl OscDebugTracker {
    pub(super) fn from_env() -> Self {
        Self {
            enabled: osc_debug_enabled_from_env(),
            state: Osc52ForwarderState::Ground,
            body: Vec::new(),
            pending: Vec::new(),
        }
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }

        for &byte in bytes {
            match self.state {
                Osc52ForwarderState::Ground => {
                    if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    }
                }
                Osc52ForwarderState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = Osc52ForwarderState::OscBody;
                    } else if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    } else {
                        self.state = Osc52ForwarderState::Ground;
                    }
                }
                Osc52ForwarderState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    }
                    0x1b => self.state = Osc52ForwarderState::OscEscape,
                    _ => self.body.push(byte),
                },
                Osc52ForwarderState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = Osc52ForwarderState::OscBody;
                    }
                }
            }

            if self.body.len() > 4096 {
                self.body.clear();
                self.state = Osc52ForwarderState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some(event) = parse_osc_debug_event(&self.body) {
            self.pending.push(event);
        }
        self.body.clear();
    }

    pub(super) fn drain_pending(&mut self) -> Vec<OscDebugEvent> {
        std::mem::take(&mut self.pending)
    }
}

impl Default for OscDebugTracker {
    fn default() -> Self {
        Self::from_env()
    }
}

fn osc_debug_enabled_from_env() -> bool {
    std::env::var("HERDR_DEBUG_OSC_EVIDENCE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parse_osc_debug_event(body: &[u8]) -> Option<OscDebugEvent> {
    let separator = body.iter().position(|byte| *byte == b';')?;
    let command = &body[..separator];
    let payload = &body[separator + 1..];
    if !matches!(command, b"0" | b"2" | b"9" | b"21337") {
        return None;
    }
    Some(OscDebugEvent {
        command: std::str::from_utf8(command).ok()?.to_string(),
        payload: sanitized_osc_debug_payload(payload),
    })
}

fn sanitized_osc_debug_payload(payload: &[u8]) -> String {
    const MAX_CHARS: usize = 512;
    let text = String::from_utf8_lossy(payload);
    let mut sanitized = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(MAX_CHARS) {
        sanitized.push(ch);
    }
    if text.chars().count() > MAX_CHARS {
        sanitized.push_str("...");
    }
    sanitized
}

fn parse_file_uri_cwd(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else if let Some(slash) = rest.find('/') {
        let host = &rest[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        &rest[slash..]
    } else {
        rest
    };
    let path = percent_decode_utf8(path)?;

    #[cfg(windows)]
    {
        let mut path = path;
        if path.len() >= 3
            && path.as_bytes()[0] == b'/'
            && path.as_bytes()[2] == b':'
            && path.as_bytes()[1].is_ascii_alphabetic()
        {
            path.remove(0);
        }
        Some(PathBuf::from(path.replace('/', "\\")))
    }

    #[cfg(not(windows))]
    Some(PathBuf::from(path))
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            let hi = *bytes.get(idx + 1)?;
            let lo = *bytes.get(idx + 2)?;
            output.push(hex_value(hi)? * 16 + hex_value(lo)?);
            idx += 3;
        } else {
            output.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Accepts `52;c;<base64>` and `52;;<base64>`.
/// Queries (`?`) are rejected because herdr has no reply path.
/// The payload must decode as base64 before it is forwarded.
fn parse_osc52_clipboard_write(body: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;

    let rest = body.strip_prefix(b"52;")?;
    let sep = rest.iter().position(|b| *b == b';')?;
    let selector = &rest[..sep];
    let data = &rest[sep + 1..];
    if !(selector.is_empty() || selector == b"c") || data == b"?" {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn foreground_job_is_shell(job: &crate::platform::ForegroundJob, shell_pid: u32) -> bool {
    job.processes.iter().any(|process| process.pid == shell_pid)
}

pub(super) fn current_transient_default_color_owner(shell_pid: u32) -> Option<u32> {
    let job = crate::detect::foreground_job(shell_pid)?;
    (!foreground_job_is_shell(&job, shell_pid)).then_some(job.process_group_id)
}

fn foreground_job_uses_droid_scrollback_compat(job: &crate::platform::ForegroundJob) -> bool {
    job.processes.iter().any(|process| {
        process.name.eq_ignore_ascii_case("droid")
            || process
                .argv0
                .as_deref()
                .is_some_and(|argv0| argv0.eq_ignore_ascii_case("droid"))
            || process.cmdline.as_deref().is_some_and(|cmdline| {
                cmdline.eq_ignore_ascii_case("droid")
                    || cmdline.starts_with("droid ")
                    || cmdline.to_ascii_lowercase().contains("/droid")
            })
    })
}

pub(super) fn contains_scrollback_clear_sequence(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\x1b[3J")
        || bytes.windows(5).any(|window| window == b"\x1b[?3J")
}

fn strip_scrollback_clear_sequences<'a>(bytes: &'a [u8]) -> Cow<'a, [u8]> {
    if !contains_scrollback_clear_sequence(bytes) {
        return Cow::Borrowed(bytes);
    }

    let mut filtered = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let remaining = &bytes[index..];
        if remaining.starts_with(b"\x1b[3J") {
            index += 4;
            continue;
        }
        if remaining.starts_with(b"\x1b[?3J") {
            index += 5;
            continue;
        }
        filtered.push(bytes[index]);
        index += 1;
    }

    Cow::Owned(filtered)
}

pub(super) fn maybe_filter_primary_screen_scrollback_clear<'a>(
    bytes: &'a [u8],
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> Cow<'a, [u8]> {
    // Droid redraws its primary-screen TUI with CSI 3 J, which erases pane
    // scrollback inside herdr. Keep the hack scoped to Droid on the primary
    // screen so normal terminal clear-history behavior still works elsewhere.
    if alternate_screen
        || !contains_scrollback_clear_sequence(bytes)
        || !foreground_job.is_some_and(foreground_job_uses_droid_scrollback_compat)
    {
        return Cow::Borrowed(bytes);
    }

    strip_scrollback_clear_sequences(bytes)
}

#[cfg(target_os = "macos")]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    let _ = owner_pgid;
    foreground_job_is_shell(foreground_job, shell_pid)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    foreground_job.process_group_id != owner_pgid
        && foreground_job_is_shell(foreground_job, shell_pid)
}

pub(super) fn write_host_terminal_theme(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
) {
    write_host_terminal_theme_selective(terminal, theme, true, true);
}

pub(super) fn write_host_terminal_theme_selective(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
    foreground: bool,
    background: bool,
) {
    if foreground {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Foreground,
            theme.foreground,
        );
    }
    if background {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Background,
            theme.background,
        );
    }
}

fn write_host_default_color(
    terminal: &mut crate::ghostty::Terminal,
    kind: crate::terminal_theme::DefaultColorKind,
    color: Option<crate::terminal_theme::RgbColor>,
) {
    let sequence = if let Some(color) = color {
        crate::terminal_theme::osc_set_default_color_sequence(kind, color)
    } else {
        crate::terminal_theme::osc_reset_default_color_sequence(kind).to_string()
    };
    terminal.write(sequence.as_bytes());
}

pub(super) fn restore_host_terminal_theme_if_needed(
    core: &mut GhosttyPaneCore,
    pane_id: PaneId,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    let Some(owner_pgid) = core.transient_default_color_owner_pgid else {
        return false;
    };
    if core.host_terminal_theme.is_empty() {
        return false;
    }
    if !should_restore_host_terminal_theme(owner_pgid, shell_pid, alternate_screen, foreground_job)
    {
        return false;
    }

    core.transient_default_color_owner_pgid = None;
    core.child_default_foreground_changed = false;
    core.child_default_background_changed = false;
    write_host_terminal_theme(&mut core.terminal, core.host_terminal_theme);
    info!(
        pane = pane_id.raw(),
        owner_pgid, "restored host terminal default colors after transient override"
    );
    true
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::layout::PaneId;

    fn pane_default_theme(
        pane: &super::super::GhosttyPaneTerminal,
    ) -> crate::terminal_theme::TerminalTheme {
        let mut core = pane.core.lock().unwrap();
        let super::super::terminal::GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        render_state.update(terminal).unwrap();
        let colors = render_state.colors().unwrap();
        crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: colors.foreground.r,
                g: colors.foreground.g,
                b: colors.foreground.b,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: colors.background.r,
                g: colors.background.g,
                b: colors.background.b,
            }),
        }
    }

    fn shell_job(shell_pid: u32) -> crate::platform::ForegroundJob {
        crate::platform::ForegroundJob {
            process_group_id: shell_pid,
            processes: vec![crate::platform::ForegroundProcess {
                pid: shell_pid,
                name: "zsh".to_string(),
                argv0: Some("zsh".to_string()),
                argv: Some(vec!["zsh".to_string()]),
                cmdline: Some("zsh".to_string()),
            }],
        }
    }

    fn tracked_default_color_events(
        events: Vec<DefaultColorTrackedEvent>,
    ) -> Vec<DefaultColorEvent> {
        events.into_iter().map(|event| event.event).collect()
    }

    fn enabled_osc_debug_tracker() -> OscDebugTracker {
        OscDebugTracker {
            enabled: true,
            state: Osc52ForwarderState::Ground,
            body: Vec::new(),
            pending: Vec::new(),
        }
    }

    #[test]
    fn default_color_tracker_detects_split_osc_11_sequences() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]11;rgb:11/22"));
        assert!(tracker.observe(b"/33\x1b\\"));
    }

    #[test]
    fn default_color_tracker_ignores_osc_queries() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]10;?\x1b\\"));
        assert!(!tracker.observe(b"\x1b]11;?\x07"));
    }

    #[test]
    fn cwd_osc_tracker_detects_split_osc7_sequence() {
        let mut tracker = CwdOscTracker::default();

        tracker.observe(b"\x1b]7;file:///tmp/herdr%20repo");
        assert_eq!(tracker.drain_latest(), None);
        tracker.observe(b"\x07");

        assert_eq!(
            tracker.drain_latest(),
            Some(std::path::PathBuf::from("/tmp/herdr repo"))
        );
    }

    #[test]
    fn cwd_osc_tracker_detects_windows_terminal_cwd_sequence() {
        let mut tracker = CwdOscTracker::default();

        tracker.observe(b"\x1b]9;9;C:\\Users\\herdr\\src\\herdr\x1b\\");

        assert_eq!(
            tracker.drain_latest(),
            Some(std::path::PathBuf::from("C:\\Users\\herdr\\src\\herdr"))
        );
    }

    // The quoted form is what Windows Terminal's documented shell integration
    // snippet emits. Herdr's own injected prompt integration deliberately
    // emits the path unquoted; see WINDOWS_POWERSHELL_SHELL_INTEGRATION_COMMAND.
    #[test]
    fn cwd_osc_tracker_detects_quoted_powershell_prompt_cwd_sequence() {
        let mut tracker = CwdOscTracker::default();

        tracker.observe(b"PS C:\\my proj> \x1b]9;9;\"C:\\my proj\"\x1b\\");

        assert_eq!(
            tracker.drain_latest(),
            Some(std::path::PathBuf::from("C:\\my proj"))
        );
    }

    // -----------------------------------------------------------------------
    // AgentOscStateTracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn agent_osc_osc0_title_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]0;braille title\x07world".as_bytes());
        assert_eq!(t.latest_title(), "braille title");
        assert_eq!(t.terminal_title(), Some("braille title"));
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_osc2_title_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]2;static title\x1b\\world".as_bytes());
        assert_eq!(t.latest_title(), "static title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_empty_osc0_clears_title() {
        let mut t = AgentOscStateTracker::default();
        // First set a title.
        t.observe(b"\x1b]0;some title\x07");
        assert_eq!(t.latest_title(), "some title");
        // Then clear it with an empty payload (Codex pattern).
        t.observe(b"\x1b]0;\x07");
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.terminal_title(), None);
    }

    #[test]
    fn clearing_agent_evidence_preserves_the_terminal_title() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe("\x1b]2;✳ 修复🙂标题\x1b\\".as_bytes());

        tracker.clear_retained();

        assert_eq!(tracker.latest_title(), "");
        assert_eq!(tracker.terminal_title(), Some("✳ 修复🙂标题"));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_seed_does_not_restore_agent_detection_evidence() {
        let mut tracker = AgentOscStateTracker::default();

        tracker.seed_terminal_title(Some("✳ restored title".into()));

        assert_eq!(tracker.terminal_title(), Some("✳ restored title"));
        assert_eq!(tracker.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_sets_progress_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        assert_eq!(t.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_clear_progress_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        t.observe(b"\x1b]9;4;0;\x1b\\");
        assert_eq!(t.latest_progress(), "4;0;");
    }

    #[test]
    fn agent_osc_split_sequence_across_chunks() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3");
        assert_eq!(t.latest_progress(), "");
        t.observe(b";\x07");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_bel_and_st_terminators_both_work() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;title-bel\x07");
        assert_eq!(t.latest_title(), "title-bel");
        t.observe(b"\x1b]0;title-st\x1b\\");
        assert_eq!(t.latest_title(), "title-st");
    }

    #[test]
    fn agent_osc_oversized_payload_is_discarded_and_recovers() {
        let mut t = AgentOscStateTracker::default();
        // Set a title first.
        t.observe(b"\x1b]0;before\x07");
        assert_eq!(t.latest_title(), "before");

        // Feed an oversized OSC body (> 4096 bytes).
        let mut oversized = Vec::from(b"\x1b]0;".as_slice());
        oversized.extend(std::iter::repeat_n(b'x', 4097));
        oversized.push(0x07);
        t.observe(&oversized);
        // The oversized body is dropped; the previously stored title is kept.
        assert_eq!(t.latest_title(), "before");

        // After recovery, subsequent valid sequences are captured normally.
        t.observe(b"\x1b]0;after\x07");
        assert_eq!(t.latest_title(), "after");
    }

    #[test]
    fn agent_osc_cap_length_is_respected() {
        let mut t = AgentOscStateTracker::default();
        // Build a title of AGENT_OSC_MAX_CHARS + 50 ASCII chars.
        let long_title: String = "a".repeat(AGENT_OSC_MAX_CHARS + 50);
        let seq = format!("\x1b]0;{long_title}\x07");
        t.observe(seq.as_bytes());
        assert_eq!(t.latest_title().len(), AGENT_OSC_MAX_CHARS);
    }

    #[test]
    fn agent_osc_control_chars_stripped() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;before\x01after\x07");
        assert_eq!(t.latest_title(), "beforeafter");
    }

    #[test]
    fn agent_osc_unrelated_osc_does_not_overwrite_title() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;my title\x07");
        // OSC 4 (palette color), OSC 52 (clipboard) — should not touch title/progress.
        t.observe(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        t.observe(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.latest_title(), "my title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_interleaved_sequences() {
        let mut t = AgentOscStateTracker::default();
        // OSC 0 title, then OSC 9 progress, then OSC 2 title update.
        t.observe(b"\x1b]0;first\x07\x1b]9;4;3;\x07\x1b]2;second\x07");
        assert_eq!(t.latest_title(), "second");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_default_state_is_empty() {
        let t = AgentOscStateTracker::default();
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.latest_progress(), "");
    }

    // -----------------------------------------------------------------------
    // OscDebugTracker tests (existing)
    // -----------------------------------------------------------------------

    #[test]
    fn osc_debug_tracker_detects_title_with_bel() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]0;✻ working title\x07world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "✻ working title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_title_with_st() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]2;static title\x1b\\world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "2".to_string(),
                payload: "static title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_split_status_sequences() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]9;4;3");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\x07\x1b]21337;status=working\x1b\\");

        assert_eq!(
            tracker.drain_pending(),
            vec![
                OscDebugEvent {
                    command: "9".to_string(),
                    payload: "4;3".to_string(),
                },
                OscDebugEvent {
                    command: "21337".to_string(),
                    payload: "status=working".to_string(),
                },
            ]
        );
    }

    #[test]
    fn osc_debug_tracker_ignores_untracked_osc_commands() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]52;c;SGVsbG8=\x07\x1b]7;file:///tmp\x07");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn osc_debug_tracker_sanitizes_control_characters() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]0;before\x01after\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "beforeafter".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_recovers_after_oversized_payload() {
        let mut tracker = enabled_osc_debug_tracker();
        let oversized = vec![b'a'; 4097];

        tracker.observe(b"\x1b]0;");
        tracker.observe(&oversized);
        tracker.observe(b"\x07\x1b]0;ok\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "ok".to_string(),
            }]
        );
    }

    #[test]
    fn default_color_event_tracker_detects_queries_sets_and_resets() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(
            b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07\x1b]4;0;?\x07\x1b]10;rgb:11/22/33\x07\x1b]111\x07",
        );

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![
                DefaultColorEvent::Query(DefaultColorQuery::Foreground),
                DefaultColorEvent::Query(DefaultColorQuery::Background),
                DefaultColorEvent::Query(DefaultColorQuery::Cursor),
                DefaultColorEvent::PaletteQuery(0),
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
                DefaultColorEvent::Reset(DefaultColorQuery::Background),
            ]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_default_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]11");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b";?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;25");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"5;?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(255)]
        );
    }

    #[test]
    fn default_color_event_tracker_rejects_malformed_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;;?\x07");
        tracker.observe(b"\x1b]4;-1;?\x07");
        tracker.observe(b"\x1b]4;256;?\x07");
        tracker.observe(b"\x1b]4;0;?;1;?\x07");
        tracker.observe(b"\x1b]4;0;rgb:1111/2222/3333\x07");
        tracker.observe(b"\x1b]4;0;?\x07");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(0)]
        );
    }

    #[test]
    fn default_color_event_tracker_ignores_other_osc_and_dcs_payloads() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]0;title\x07");
        tracker.observe(b"\x1b]52;c;?\x07");
        tracker.observe(b"\x1bPtmux;\x1b\x1b]11;?\x07\x1b\\");
        tracker.observe(b"\x1bPtmux;payload\x07\x1b]11;?\x07\x1b\\");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn default_color_event_tracker_ignores_oversized_osc_until_terminator() {
        let mut tracker = DefaultColorEventTracker::default();
        let mut oversized = Vec::from(b"\x1b]11;".as_slice());
        oversized.extend(std::iter::repeat_n(b'a', 1025));
        oversized.extend_from_slice(b"\x1b]11;?\x07");

        tracker.observe(&oversized);
        assert!(tracker.drain_pending().is_empty());

        tracker.observe(b"\x1b]11;?\x07");
        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn osc52_forwarder_detects_write_with_bel() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVsbG8=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_detects_write_with_st() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVsbG8=\x1b\\");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_detects_empty_selector_form() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;;aGVsbG8=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_accepts_clear_clipboard() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn osc52_forwarder_ignores_query() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;?\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_empty_selector_query() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;;?\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_other_kinds() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;p;aGk=\x07");
        fw.observe(b"\x1b]52;s;aGk=\x07");
        fw.observe(b"\x1b]52;q;aGk=\x07");
        fw.observe(b"\x1b]52;0;aGk=\x07");
        fw.observe(b"\x1b]52;7;aGk=\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_invalid_base64() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;%%%\x07");
        fw.observe(b"\x1b]52;c;aGVs\x1b[bG8=\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_non_osc52() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]11;?\x07");
        fw.observe(b"\x1b]0;title\x07");
        fw.observe(b"\x1b]8;;https://example.com\x1b\\");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_handles_split_sequence_mid_payload() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVs");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"bG8gd29y");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"bGQ=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_handles_split_before_bel() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_handles_split_between_esc_and_backslash() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x1b");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"\\");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_payload_size_limit() {
        let mut fw = Osc52Forwarder::default();
        let mut huge = Vec::with_capacity(OSC52_MAX_PAYLOAD_BYTES + 32);
        huge.extend_from_slice(b"\x1b]52;c;");
        huge.extend(std::iter::repeat_n(b'A', OSC52_MAX_PAYLOAD_BYTES + 16));
        huge.push(0x07);
        fw.observe(&huge);
        assert!(fw.drain_pending().is_empty());

        fw.observe(b"\x1b]52;c;aGk=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_recovers_after_garbage() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x01\x02random\x7fbytes\x1b]52;c;aGk=\x07tail");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_multiple_in_one_chunk() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x07\x1b]52;c;Ynll\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec(), b"bye".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_drain_clears_pending() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x07");
        assert_eq!(fw.drain_pending(), vec![b"hi".to_vec()]);
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn droid_scrollback_compat_matches_process_name_and_cmdline() {
        let name_only = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "droid".to_string(),
                argv0: None,
                argv: Some(vec![
                    "/opt/factory/droid".to_string(),
                    "--resume".to_string(),
                ]),
                cmdline: Some("/opt/factory/droid --resume".to_string()),
            }],
        };
        assert!(foreground_job_uses_droid_scrollback_compat(&name_only));

        let cmdline_only = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "bun".to_string(),
                argv0: Some("bun".to_string()),
                argv: Some(vec![
                    "bun".to_string(),
                    "/home/can/.local/bin/droid".to_string(),
                    "--resume".to_string(),
                ]),
                cmdline: Some("/home/can/.local/bin/droid --resume".to_string()),
            }],
        };
        assert!(foreground_job_uses_droid_scrollback_compat(&cmdline_only));

        let shell = shell_job(7);
        assert!(!foreground_job_uses_droid_scrollback_compat(&shell));
    }

    #[test]
    fn strip_scrollback_clear_sequences_removes_ed3_only() {
        let filtered = strip_scrollback_clear_sequences(b"a\x1b[3Jb\x1b[?3Jc\x1b[2Jd");
        assert_eq!(filtered.as_ref(), b"abc\x1b[2Jd");
    }

    #[test]
    fn primary_screen_droid_compat_ignores_scrollback_clear_only_for_droid() {
        let droid_job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "droid".to_string(),
                argv0: Some("droid".to_string()),
                argv: Some(vec!["droid".to_string()]),
                cmdline: Some("droid".to_string()),
            }],
        };

        let filtered = maybe_filter_primary_screen_scrollback_clear(
            b"\x1b[3J\x1b[2J",
            false,
            Some(&droid_job),
        );
        assert_eq!(filtered.as_ref(), b"\x1b[2J");

        let shell = maybe_filter_primary_screen_scrollback_clear(
            b"\x1b[3J\x1b[2J",
            false,
            Some(&shell_job(7)),
        );
        assert_eq!(shell.as_ref(), b"\x1b[3J\x1b[2J");

        let alternate =
            maybe_filter_primary_screen_scrollback_clear(b"\x1b[3J\x1b[2J", true, Some(&droid_job));
        assert_eq!(alternate.as_ref(), b"\x1b[3J\x1b[2J");
    }

    #[test]
    fn host_theme_restore_waits_for_shell_and_non_alternate_screen() {
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            true,
            Some(&shell_job(7)),
        ));
        assert!(!should_restore_host_terminal_theme(42, 7, false, None));
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&crate::platform::ForegroundJob {
                process_group_id: 42,
                processes: vec![crate::platform::ForegroundProcess {
                    pid: 42,
                    name: "droid".to_string(),
                    argv0: Some("droid".to_string()),
                    argv: Some(vec!["droid".to_string()]),
                    cmdline: Some("droid".to_string()),
                }],
            }),
        ));
        assert!(should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(target_os = "macos")]
        assert!(should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(not(target_os = "macos"))]
        assert!(!should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));
    }

    #[test]
    fn restore_host_terminal_theme_reapplies_cached_colors() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = super::super::GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let pane_id = PaneId::from_raw(1);
        let shell_pid = 7;
        let host_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x11,
                g: 0x22,
                b: 0x33,
            }),
        };

        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.terminal.write(b"\x1b]11;rgb:dd/ee/ff\x1b\\");
        }
        assert_eq!(
            pane_default_theme(&pane).background,
            Some(crate::terminal_theme::RgbColor {
                r: 0xdd,
                g: 0xee,
                b: 0xff,
            })
        );

        {
            let mut core = pane.core.lock().unwrap();
            assert!(restore_host_terminal_theme_if_needed(
                &mut core,
                pane_id,
                shell_pid,
                false,
                Some(&shell_job(shell_pid)),
            ));
        }

        assert_eq!(pane_default_theme(&pane).background, host_theme.background);
        assert_eq!(pane_default_theme(&pane).foreground, host_theme.foreground);
    }
}
