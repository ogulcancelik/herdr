#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use tokio::sync::mpsc;

use super::windows_client_input_event_from_raw;
#[cfg(windows)]
use super::ClientLoopEvent;

#[cfg(windows)]
pub(super) fn raw_console_reader_loop(
    handle: windows_sys::Win32::Foundation::HANDLE,
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    let mut mapper = WindowsInputMapper::default();
    let mut pump = WindowsInputPump::default();

    while !should_quit.load(Ordering::Acquire) {
        match windows_console_input_items(handle, &mut mapper) {
            WindowsInputItems::Items(items) => {
                if !process_platform_input_items(items, &mut pump, &event_tx) {
                    return;
                }
            }
            WindowsInputItems::Idle => {
                if !process_platform_input_items(mapper.idle(), &mut pump, &event_tx) {
                    return;
                }
                if !send_windows_client_input_events(pump.idle(), &event_tx) {
                    return;
                }
            }
            WindowsInputItems::Closed => return,
        }
    }
}

#[cfg(windows)]
fn process_platform_input_items(
    items: Vec<PlatformInputItem>,
    pump: &mut WindowsInputPump,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
) -> bool {
    for item in items {
        if !send_windows_client_input_events(pump.process(item), event_tx) {
            return false;
        }
    }
    true
}

#[cfg(windows)]
pub(super) fn console_input_handle() -> std::io::Result<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(handle)
    }
}

#[cfg(windows)]
pub(super) fn virtual_terminal_input_enabled(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> bool {
    use windows_sys::Win32::System::Console::{GetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT};

    let mut mode = 0;
    (unsafe { GetConsoleMode(handle, &mut mode) } != 0) && mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0
}

#[cfg(windows)]
enum WindowsInputItems {
    Items(Vec<PlatformInputItem>),
    Idle,
    Closed,
}

#[cfg(windows)]
fn windows_console_input_items(
    handle: windows_sys::Win32::Foundation::HANDLE,
    mapper: &mut WindowsInputMapper,
) -> WindowsInputItems {
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    match unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(handle, 10) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return WindowsInputItems::Idle,
        _ => return WindowsInputItems::Closed,
    }

    let mut records = [windows_sys::Win32::System::Console::INPUT_RECORD::default(); 64];
    let mut read = 0;
    let ok = unsafe {
        windows_sys::Win32::System::Console::ReadConsoleInputW(
            handle,
            records.as_mut_ptr(),
            records.len() as u32,
            &mut read,
        )
    };
    if ok == 0 {
        return WindowsInputItems::Closed;
    }

    let mut items = Vec::new();
    for record in records.iter().take(read as usize) {
        if let Some(record) = windows_console_input_record_from_os(*record) {
            items.extend(mapper.translate(record));
        }
    }
    WindowsInputItems::Items(items)
}

#[cfg(windows)]
fn windows_console_input_record_from_os(
    record: windows_sys::Win32::System::Console::INPUT_RECORD,
) -> Option<WindowsInputRecord> {
    use windows_sys::Win32::System::Console::{FOCUS_EVENT, KEY_EVENT, MOUSE_EVENT};

    match record.EventType as u32 {
        KEY_EVENT => {
            let key = unsafe { record.Event.KeyEvent };
            let unicode = unsafe { key.uChar.UnicodeChar };
            Some(WindowsInputRecord::Key(WindowsKeyRecord {
                key_down: key.bKeyDown != 0,
                repeat_count: key.wRepeatCount,
                virtual_key_code: key.wVirtualKeyCode,
                virtual_scan_code: key.wVirtualScanCode,
                unicode,
                control_key_state: key.dwControlKeyState,
            }))
        }
        MOUSE_EVENT => {
            let mouse = unsafe { record.Event.MouseEvent };
            Some(WindowsInputRecord::Mouse(WindowsMouseRecord {
                x: mouse.dwMousePosition.X.max(0) as u16,
                y: mouse.dwMousePosition.Y.max(0) as u16,
                button_state: mouse.dwButtonState,
                control_key_state: mouse.dwControlKeyState,
                event_flags: mouse.dwEventFlags,
            }))
        }
        FOCUS_EVENT => {
            let focus = unsafe { record.Event.FocusEvent };
            Some(WindowsInputRecord::Focus(focus.bSetFocus != 0))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
enum WindowsInputRecord {
    Key(WindowsKeyRecord),
    Mouse(WindowsMouseRecord),
    Focus(bool),
}

#[derive(Clone, Copy, Debug)]
struct WindowsKeyRecord {
    key_down: bool,
    repeat_count: u16,
    virtual_key_code: u16,
    virtual_scan_code: u16,
    unicode: u16,
    control_key_state: u32,
}

#[derive(Clone, Copy, Debug)]
struct WindowsMouseRecord {
    x: u16,
    y: u16,
    button_state: u32,
    control_key_state: u32,
    event_flags: u32,
}

#[derive(Default)]
struct WindowsInputMapper {
    pending_high_surrogate: Option<u16>,
    pending_paste_high_surrogate: Option<u16>,
    mouse_buttons: WindowsMouseButtons,
    win32_input: WindowsWin32InputModeFramer,
}

#[derive(Default)]
struct WindowsInputPump {
    framer: crate::raw_input::RawInputFramer,
    paste_from_win32_key_records: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlatformInputItem {
    Bytes(Vec<u8>),
    Semantic(crate::protocol::ClientInputEvent),
    PasteAwareBytes {
        paste_bytes: Vec<u8>,
        raw_bytes: Vec<u8>,
        win32_paste_bytes: Vec<u8>,
    },
    PasteAwareKey {
        bytes: Vec<u8>,
        win32_paste_bytes: Vec<u8>,
        events: Vec<crate::protocol::ClientInputEvent>,
    },
}

#[derive(Default)]
struct WindowsWin32InputModeFramer {
    buffer: Vec<u8>,
}

enum WindowsWin32InputModeItem {
    Bytes(Vec<u8>),
    Key {
        bytes: Vec<u8>,
        record: WindowsKeyRecord,
    },
}

impl WindowsInputPump {
    fn process(&mut self, item: PlatformInputItem) -> Vec<crate::protocol::ClientInputEvent> {
        match item {
            PlatformInputItem::Bytes(bytes) => {
                let raw_events = self.framer.push(&bytes);
                self.process_raw_events(raw_events)
            }
            PlatformInputItem::Semantic(event) => {
                let raw_events = self.framer.flush_timeout();
                let mut events = self.process_raw_events(raw_events);
                events.push(event);
                events
            }
            PlatformInputItem::PasteAwareBytes {
                paste_bytes,
                raw_bytes,
                win32_paste_bytes,
            } => {
                let pending_paste = self.framer.has_pending_bracketed_paste();
                let decode_win32_record = self.paste_from_win32_key_records || !pending_paste;
                let raw_events = if decode_win32_record {
                    if pending_paste {
                        self.framer.push(&win32_paste_bytes)
                    } else {
                        self.framer.push(&raw_bytes)
                    }
                } else {
                    self.framer.push(&paste_bytes)
                };
                if decode_win32_record && self.framer.has_pending_bracketed_paste() {
                    self.paste_from_win32_key_records = true;
                }
                self.process_raw_events(raw_events)
            }
            PlatformInputItem::PasteAwareKey {
                bytes,
                win32_paste_bytes,
                events,
            } => {
                if self.framer.has_pending_bracketed_paste() {
                    let bytes = if self.paste_from_win32_key_records {
                        &win32_paste_bytes
                    } else {
                        &bytes
                    };
                    let raw_events = self.framer.push(bytes);
                    self.process_raw_events(raw_events)
                } else {
                    let raw_events = self.framer.flush_timeout();
                    let mut output = self.process_raw_events(raw_events);
                    output.extend(events);
                    output
                }
            }
        }
    }

    fn idle(&mut self) -> Vec<crate::protocol::ClientInputEvent> {
        let raw_events = self.framer.flush_timeout();
        self.process_raw_events(raw_events)
    }

    fn process_raw_events(
        &mut self,
        events: Vec<crate::raw_input::RawInputEvent>,
    ) -> Vec<crate::protocol::ClientInputEvent> {
        if events
            .iter()
            .any(|event| matches!(event, crate::raw_input::RawInputEvent::Paste(_)))
        {
            self.paste_from_win32_key_records = false;
        }
        Self::raw_events_to_client_events(events)
    }

    fn raw_events_to_client_events(
        events: Vec<crate::raw_input::RawInputEvent>,
    ) -> Vec<crate::protocol::ClientInputEvent> {
        events
            .into_iter()
            .filter_map(windows_client_input_event_from_raw)
            .collect()
    }
}

#[cfg(test)]
#[derive(Default)]
struct WindowsInputTranslator {
    mapper: WindowsInputMapper,
    pump: WindowsInputPump,
}

#[cfg(test)]
impl WindowsInputTranslator {
    fn translate(&mut self, record: WindowsInputRecord) -> Vec<crate::protocol::ClientInputEvent> {
        let mut events = Vec::new();
        for item in self.mapper.translate(record) {
            events.extend(self.pump.process(item));
        }
        events
    }

    fn idle(&mut self) -> Vec<crate::protocol::ClientInputEvent> {
        let mut events = Vec::new();
        for item in self.mapper.idle() {
            events.extend(self.pump.process(item));
        }
        events.extend(self.pump.idle());
        events
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WindowsMouseButtons {
    left: bool,
    right: bool,
    middle: bool,
}

impl WindowsInputMapper {
    fn idle(&mut self) -> Vec<PlatformInputItem> {
        self.win32_input
            .flush_timeout()
            .into_iter()
            .map(PlatformInputItem::Bytes)
            .collect()
    }

    fn translate(&mut self, record: WindowsInputRecord) -> Vec<PlatformInputItem> {
        match record {
            WindowsInputRecord::Key(key) => self.translate_key(key),
            WindowsInputRecord::Mouse(mouse) => {
                let items = self
                    .translate_mouse(mouse)
                    .map(PlatformInputItem::Semantic)
                    .into_iter()
                    .collect();
                self.with_pending_win32_flush(items)
            }
            WindowsInputRecord::Focus(focused) => {
                self.with_pending_win32_flush(vec![PlatformInputItem::Semantic(if focused {
                    crate::protocol::ClientInputEvent::FocusGained
                } else {
                    crate::protocol::ClientInputEvent::FocusLost
                })])
            }
        }
    }

    fn translate_key(&mut self, key: WindowsKeyRecord) -> Vec<PlatformInputItem> {
        if windows_input_trace_enabled() {
            tracing::info!(
                key_down = key.key_down,
                repeat_count = key.repeat_count,
                virtual_key_code = key.virtual_key_code,
                unicode = key.unicode,
                control_key_state = key.control_key_state,
                "windows input trace: console key record"
            );
        }

        if !self.key_record_can_emit_event(key) {
            return Vec::new();
        }

        if self.key_record_is_raw_escape(key) {
            return self.translate_win32_input_mode_bytes(&[0x1b]);
        }

        let repeat_count = key.repeat_count.max(1);
        if key.virtual_key_code == 0 {
            let mut items = Vec::new();
            let mut flush_pending_win32_before_items = false;
            for repeat_idx in 0..repeat_count {
                let kind = Self::semantic_key_kind(key, false, repeat_idx);
                if let Some((bytes, event)) = self.synthetic_modified_key_event(key, kind) {
                    flush_pending_win32_before_items = true;
                    items.push(PlatformInputItem::PasteAwareKey {
                        win32_paste_bytes: bytes.clone(),
                        bytes,
                        events: vec![event],
                    });
                } else if let Some(bytes) = self.synthetic_utf16_unit_to_bytes(key.unicode) {
                    items.extend(self.translate_win32_input_mode_bytes(&bytes));
                }
            }
            return if flush_pending_win32_before_items {
                self.with_pending_win32_flush(items)
            } else {
                items
            };
        }

        let events = self.translate_semantic_key_events(key);
        let items = if let Some(bytes) = self.paste_payload_bytes_for_key(key) {
            vec![PlatformInputItem::PasteAwareKey {
                win32_paste_bytes: bytes.clone(),
                bytes,
                events,
            }]
        } else {
            events
                .into_iter()
                .map(PlatformInputItem::Semantic)
                .collect()
        };
        self.with_pending_win32_flush(items)
    }

    fn key_record_is_raw_escape(&self, key: WindowsKeyRecord) -> bool {
        let modifiers = windows_key_modifiers(key.control_key_state);
        if !key.key_down
            || key.repeat_count.max(1) != 1
            || modifiers.contains(crossterm::event::KeyModifiers::ALT)
        {
            return false;
        }

        let bare_escape = modifiers.is_empty()
            && (key.virtual_key_code == 0x1b || (key.virtual_key_code == 0 && key.unicode == 0x1b));
        let ctrl_bracket = key.virtual_key_code == 0xdb
            && key.unicode == 0x1b
            && modifiers == crossterm::event::KeyModifiers::CONTROL;

        bare_escape || ctrl_bracket
    }

    fn translate_win32_input_mode_bytes(&mut self, bytes: &[u8]) -> Vec<PlatformInputItem> {
        let mut items = Vec::new();
        for item in self.win32_input.push(bytes) {
            match item {
                WindowsWin32InputModeItem::Bytes(bytes) => {
                    items.push(PlatformInputItem::Bytes(bytes))
                }
                WindowsWin32InputModeItem::Key { bytes, record } => {
                    let win32_paste_bytes =
                        self.paste_payload_bytes_for_key(record).unwrap_or_default();
                    if let Some(raw_bytes) = self.win32_input_mode_key_record_raw_bytes(record) {
                        items.push(PlatformInputItem::PasteAwareBytes {
                            paste_bytes: bytes,
                            raw_bytes,
                            win32_paste_bytes,
                        });
                    } else {
                        items.push(PlatformInputItem::PasteAwareKey {
                            bytes,
                            win32_paste_bytes,
                            events: self.translate_semantic_key_events(record),
                        })
                    }
                }
            }
        }
        items
    }

    fn win32_input_mode_key_record_raw_bytes(
        &mut self,
        record: WindowsKeyRecord,
    ) -> Option<Vec<u8>> {
        if self.key_record_is_raw_escape(record) {
            return Some(vec![0x1b]);
        }

        if !record.key_down
            || record.repeat_count.max(1) != 1
            || windows_key_modifiers(record.control_key_state).bits() != 0
        {
            return None;
        }

        if record.virtual_key_code != 0 && (record.unicode < 0x20 || record.unicode == 0x7f) {
            return None;
        }

        self.synthetic_utf16_unit_to_bytes(record.unicode)
    }

    fn with_pending_win32_flush(
        &mut self,
        mut items: Vec<PlatformInputItem>,
    ) -> Vec<PlatformInputItem> {
        let mut pending = self.idle();
        pending.append(&mut items);
        pending
    }

    fn synthetic_modified_key_event(
        &mut self,
        key: WindowsKeyRecord,
        kind: crate::protocol::ClientKeyKind,
    ) -> Option<(Vec<u8>, crate::protocol::ClientInputEvent)> {
        use crate::protocol::ClientKeyCode;

        let modifiers = windows_key_modifiers(key.control_key_state);
        if !modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
            return None;
        }

        let code = match key.unicode {
            0x0009 => ClientKeyCode::BackTab,
            0x000d => ClientKeyCode::Enter,
            _ => return None,
        };
        self.pending_high_surrogate = None;
        self.pending_paste_high_surrogate = None;
        Some((
            vec![key.unicode as u8],
            crate::protocol::ClientInputEvent::Key {
                code,
                modifiers: modifiers.bits(),
                kind,
            },
        ))
    }

    fn translate_semantic_key_events(
        &mut self,
        key: WindowsKeyRecord,
    ) -> Vec<crate::protocol::ClientInputEvent> {
        if !self.key_record_can_emit_event(key) {
            return Vec::new();
        }
        if Self::key_record_is_modifier_only(key) {
            return Vec::new();
        }

        if key.virtual_key_code == 0 {
            if let Some((_bytes, event)) =
                self.synthetic_modified_key_event(key, crate::protocol::ClientKeyKind::Press)
            {
                return vec![event];
            }
        }

        let is_alt_code = Self::is_alt_code(key);
        (0..key.repeat_count.max(1))
            .filter_map(|repeat_idx| {
                self.translate_semantic_key_event(
                    key,
                    Self::semantic_key_kind(key, is_alt_code, repeat_idx),
                )
            })
            .collect()
    }

    fn key_record_can_emit_event(&self, key: WindowsKeyRecord) -> bool {
        key.key_down || Self::is_alt_code(key) || key.virtual_key_code != 0
    }

    fn key_record_is_modifier_only(key: WindowsKeyRecord) -> bool {
        matches!(
            key.virtual_key_code,
            0x10 | 0x11 | 0x12 | 0xa0 | 0xa1 | 0xa2 | 0xa3 | 0xa4 | 0xa5
        ) && key.unicode == 0
    }

    fn is_alt_code(key: WindowsKeyRecord) -> bool {
        const VK_MENU: u16 = 0x12;
        key.virtual_key_code == VK_MENU && !key.key_down && key.unicode != 0
    }

    fn semantic_key_kind(
        key: WindowsKeyRecord,
        is_alt_code: bool,
        repeat_idx: u16,
    ) -> crate::protocol::ClientKeyKind {
        if is_alt_code {
            crate::protocol::ClientKeyKind::Press
        } else if !key.key_down {
            crate::protocol::ClientKeyKind::Release
        } else if repeat_idx > 0 {
            crate::protocol::ClientKeyKind::Repeat
        } else {
            crate::protocol::ClientKeyKind::Press
        }
    }

    fn translate_semantic_key_event(
        &mut self,
        key: WindowsKeyRecord,
        kind: crate::protocol::ClientKeyKind,
    ) -> Option<crate::protocol::ClientInputEvent> {
        let modifiers = windows_key_modifiers(key.control_key_state);
        if modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
            && key.unicode == 0x000a
            && (key.virtual_key_code == 0x4a || key.virtual_scan_code == 0x24)
        {
            self.pending_high_surrogate = None;
            return Some(crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('j'),
                modifiers: modifiers.bits(),
                kind,
            });
        }

        let code = if let Some(code) =
            windows_virtual_key_to_key_code(key.virtual_key_code, modifiers)
        {
            self.pending_high_surrogate = None;
            Some(code)
        } else {
            if key.unicode == 0 {
                self.pending_high_surrogate = None;
            }
            if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                if let Some(code) = windows_unicode_control_to_key_code(key.unicode) {
                    self.pending_high_surrogate = None;
                    return Some(crate::protocol::ClientInputEvent::Key {
                        code,
                        modifiers: modifiers.bits(),
                        kind,
                    });
                }
            }
            self.utf16_unit_to_char(key.unicode)
                .filter(|ch| !ch.is_control())
                .map(crate::protocol::ClientKeyCode::Char)
                .or_else(|| {
                    windows_virtual_key_to_char_code(key.virtual_key_code, key.unicode, modifiers)
                })
        };

        code.map(|code| crate::protocol::ClientInputEvent::Key {
            code,
            modifiers: modifiers.bits(),
            kind,
        })
    }

    fn utf16_unit_to_char(&mut self, unit: u16) -> Option<char> {
        Self::utf16_unit_to_char_with_pending(&mut self.pending_high_surrogate, unit)
    }

    fn utf16_unit_to_char_with_pending(
        pending_high_surrogate: &mut Option<u16>,
        unit: u16,
    ) -> Option<char> {
        if unit == 0 {
            return None;
        }

        if (0xd800..=0xdbff).contains(&unit) {
            *pending_high_surrogate = Some(unit);
            return None;
        }

        let ch = if (0xdc00..=0xdfff).contains(&unit) {
            let high = pending_high_surrogate.take()?;
            let codepoint = 0x10000 + (((high as u32 - 0xd800) << 10) | (unit as u32 - 0xdc00));
            char::from_u32(codepoint)?
        } else {
            *pending_high_surrogate = None;
            char::from_u32(unit as u32)?
        };

        Some(ch)
    }

    fn paste_payload_bytes_for_key(&mut self, key: WindowsKeyRecord) -> Option<Vec<u8>> {
        if key.unicode == 0 {
            return None;
        }
        if !key.key_down {
            return Some(Vec::new());
        }

        let ch = Self::utf16_unit_to_char_with_pending(
            &mut self.pending_paste_high_surrogate,
            key.unicode,
        )?;
        let mut buf = [0; 4];
        Some(
            ch.encode_utf8(&mut buf)
                .as_bytes()
                .repeat(key.repeat_count.max(1) as usize),
        )
    }

    fn synthetic_utf16_unit_to_bytes(&mut self, unit: u16) -> Option<Vec<u8>> {
        let ch = self.utf16_unit_to_char(unit)?;
        let mut buf = [0; 4];
        Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
    }

    fn translate_mouse(
        &mut self,
        mouse: WindowsMouseRecord,
    ) -> Option<crate::protocol::ClientInputEvent> {
        use crossterm::event::{MouseButton, MouseEventKind};

        const FROM_LEFT_1ST_BUTTON_PRESSED: u32 = 0x0001;
        const RIGHTMOST_BUTTON_PRESSED: u32 = 0x0002;
        const FROM_LEFT_2ND_BUTTON_PRESSED: u32 = 0x0004;
        const MOUSE_MOVED: u32 = 0x0001;
        const MOUSE_WHEELED: u32 = 0x0004;
        const MOUSE_HWHEELED: u32 = 0x0008;

        let buttons = WindowsMouseButtons {
            left: mouse.button_state & FROM_LEFT_1ST_BUTTON_PRESSED != 0,
            right: mouse.button_state & RIGHTMOST_BUTTON_PRESSED != 0,
            middle: mouse.button_state & FROM_LEFT_2ND_BUTTON_PRESSED != 0,
        };

        let kind = if mouse.event_flags & MOUSE_WHEELED != 0 {
            if (mouse.button_state as i32) < 0 {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            }
        } else if mouse.event_flags & MOUSE_HWHEELED != 0 {
            if (mouse.button_state as i32) < 0 {
                MouseEventKind::ScrollLeft
            } else {
                MouseEventKind::ScrollRight
            }
        } else if mouse.event_flags & MOUSE_MOVED != 0 {
            if buttons.left {
                MouseEventKind::Drag(MouseButton::Left)
            } else if buttons.right {
                MouseEventKind::Drag(MouseButton::Right)
            } else if buttons.middle {
                MouseEventKind::Drag(MouseButton::Middle)
            } else {
                MouseEventKind::Moved
            }
        } else if buttons.left && !self.mouse_buttons.left {
            MouseEventKind::Down(MouseButton::Left)
        } else if buttons.right && !self.mouse_buttons.right {
            MouseEventKind::Down(MouseButton::Right)
        } else if buttons.middle && !self.mouse_buttons.middle {
            MouseEventKind::Down(MouseButton::Middle)
        } else if !buttons.left && self.mouse_buttons.left {
            MouseEventKind::Up(MouseButton::Left)
        } else if !buttons.right && self.mouse_buttons.right {
            MouseEventKind::Up(MouseButton::Right)
        } else if !buttons.middle && self.mouse_buttons.middle {
            MouseEventKind::Up(MouseButton::Middle)
        } else {
            self.mouse_buttons = buttons;
            return None;
        };
        self.mouse_buttons = buttons;

        Some(crate::protocol::ClientInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::from_crossterm(kind)?,
            column: mouse.x,
            row: mouse.y,
            modifiers: windows_key_modifiers(mouse.control_key_state).bits(),
        })
    }
}

impl WindowsWin32InputModeFramer {
    fn push(&mut self, bytes: &[u8]) -> Vec<WindowsWin32InputModeItem> {
        self.buffer.extend_from_slice(bytes);

        let mut items = Vec::new();
        while let Some(item) = self.next_item() {
            items.push(item);
        }
        items
    }

    fn next_item(&mut self) -> Option<WindowsWin32InputModeItem> {
        if self.buffer.is_empty() {
            return None;
        }

        if self.buffer.as_slice() == b"\x1b" || self.buffer.as_slice() == b"\x1b[" {
            return None;
        }

        if self.buffer.starts_with(b"\x1b[") {
            let mut cursor = 2;
            while cursor < self.buffer.len()
                && (self.buffer[cursor].is_ascii_digit() || self.buffer[cursor] == b';')
            {
                cursor += 1;
            }

            if cursor == self.buffer.len() {
                return None;
            }

            if self.buffer[cursor] == b'_' {
                let bytes = self.buffer[..=cursor].to_vec();
                let body = String::from_utf8_lossy(&self.buffer[2..cursor]);
                let item = parse_win32_input_mode_key_record(&body).map(|record| {
                    WindowsWin32InputModeItem::Key {
                        bytes: bytes.clone(),
                        record,
                    }
                });
                self.buffer.drain(..=cursor);
                return Some(item.unwrap_or(WindowsWin32InputModeItem::Bytes(bytes)));
            }
        }

        Some(WindowsWin32InputModeItem::Bytes(vec![self
            .buffer
            .remove(0)]))
    }

    fn flush_timeout(&mut self) -> Vec<Vec<u8>> {
        if self.buffer.is_empty() {
            Vec::new()
        } else {
            vec![std::mem::take(&mut self.buffer)]
        }
    }
}

fn parse_win32_input_mode_key_record(body: &str) -> Option<WindowsKeyRecord> {
    let mut fields = body.split(';');
    let virtual_key_code = fields.next()?.parse::<u16>().ok()?;
    let virtual_scan_code = fields.next()?.parse::<u16>().ok()?;
    let unicode = fields.next()?.parse::<u16>().ok()?;
    let key_down = fields.next()?.parse::<u16>().ok()? != 0;
    let control_key_state = fields.next()?.parse::<u32>().ok()?;
    let repeat_count = fields.next()?.parse::<u16>().ok()?;
    if fields.next().is_some() {
        return None;
    }

    Some(WindowsKeyRecord {
        key_down,
        repeat_count,
        virtual_key_code,
        virtual_scan_code,
        unicode,
        control_key_state,
    })
}

fn windows_key_modifiers(control_key_state: u32) -> crossterm::event::KeyModifiers {
    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;
    const SHIFT_PRESSED: u32 = 0x0010;

    let mut modifiers = crossterm::event::KeyModifiers::empty();
    let alt_gr = control_key_state & RIGHT_ALT_PRESSED != 0
        && control_key_state & LEFT_CTRL_PRESSED != 0
        && control_key_state & RIGHT_CTRL_PRESSED == 0;
    if control_key_state & SHIFT_PRESSED != 0 {
        modifiers |= crossterm::event::KeyModifiers::SHIFT;
    }
    if !alt_gr && control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        modifiers |= crossterm::event::KeyModifiers::CONTROL;
    }
    // Treat right Alt as AltGr rather than a terminal Alt prefix.
    if control_key_state & LEFT_ALT_PRESSED != 0
        || (control_key_state & RIGHT_ALT_PRESSED != 0 && !alt_gr)
    {
        modifiers |= crossterm::event::KeyModifiers::ALT;
    }
    modifiers
}

fn windows_virtual_key_to_key_code(
    vk: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::protocol::ClientKeyCode> {
    use crate::protocol::ClientKeyCode;
    Some(match vk {
        0x08 => ClientKeyCode::Backspace,
        0x09 if modifiers.contains(crossterm::event::KeyModifiers::SHIFT) => ClientKeyCode::BackTab,
        0x09 => ClientKeyCode::Tab,
        0x0d => ClientKeyCode::Enter,
        0x1b => ClientKeyCode::Esc,
        0x21 => ClientKeyCode::PageUp,
        0x22 => ClientKeyCode::PageDown,
        0x23 => ClientKeyCode::End,
        0x24 => ClientKeyCode::Home,
        0x25 => ClientKeyCode::Left,
        0x26 => ClientKeyCode::Up,
        0x27 => ClientKeyCode::Right,
        0x28 => ClientKeyCode::Down,
        0x2d => ClientKeyCode::Insert,
        0x2e => ClientKeyCode::Delete,
        0x70..=0x87 => ClientKeyCode::F((vk - 0x6f) as u8),
        _ => return None,
    })
}

fn windows_virtual_key_to_char_code(
    vk: u16,
    unicode: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::protocol::ClientKeyCode> {
    use crate::protocol::ClientKeyCode;

    if let Some(ch) = char::from_u32(unicode as u32).filter(|ch| !ch.is_control()) {
        return Some(ClientKeyCode::Char(ch));
    }

    let ch = match vk {
        0x30..=0x39 => char::from_u32(vk as u32)?,
        0x41..=0x5a
            if modifiers.contains(crossterm::event::KeyModifiers::SHIFT)
                && !modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
        {
            char::from_u32(vk as u32)?
        }
        0x41..=0x5a => char::from_u32(vk as u32 + 32)?,
        _ => return None,
    };
    Some(ClientKeyCode::Char(ch))
}

fn windows_unicode_control_to_key_code(unicode: u16) -> Option<crate::protocol::ClientKeyCode> {
    use crate::protocol::ClientKeyCode;
    Some(match unicode {
        0x00 => ClientKeyCode::Char(' '),
        0x1b => ClientKeyCode::Char('['),
        0x1c => ClientKeyCode::Char('\\'),
        0x1d => ClientKeyCode::Char(']'),
        0x1e => ClientKeyCode::Char('^'),
        0x1f => ClientKeyCode::Char('-'),
        _ => return None,
    })
}

#[cfg(windows)]
fn send_windows_client_input_events(
    events: Vec<crate::protocol::ClientInputEvent>,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
) -> bool {
    if events.is_empty() {
        return true;
    }
    if windows_input_trace_enabled() {
        tracing::info!(?events, "windows input trace: client input events");
    }
    event_tx
        .blocking_send(ClientLoopEvent::StdinEvents(events))
        .is_ok()
}

#[cfg(any(windows, test))]
fn windows_input_trace_enabled() -> bool {
    std::env::var_os("HERDR_WINDOWS_INPUT_TRACE").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_char(ch: char) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: 0,
            virtual_scan_code: 0,
            unicode: ch as u16,
            control_key_state: 0,
        })
    }

    fn key_vk(vk: u16, control_key_state: u32) -> WindowsInputRecord {
        key_vk_with_unicode(vk, '\0', control_key_state)
    }

    fn key_vk_with_unicode(vk: u16, ch: char, control_key_state: u32) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: ch as u16,
            control_key_state,
        })
    }

    fn key_vk_with_utf16(vk: u16, unit: u16) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: unit,
            control_key_state: 0,
        })
    }

    fn key_vk_with_utf16_mods(vk: u16, unit: u16, control_key_state: u32) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: unit,
            control_key_state,
        })
    }

    fn key_vk_with_scan_unicode(
        vk: u16,
        scan: u16,
        ch: char,
        control_key_state: u32,
    ) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: scan,
            unicode: ch as u16,
            control_key_state,
        })
    }

    fn key_vk_with_repeat(vk: u16, repeat_count: u16) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: 0,
            control_key_state: 0,
        })
    }

    fn key_vk_with_unicode_repeat(
        vk: u16,
        ch: char,
        control_key_state: u32,
        repeat_count: u16,
    ) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: true,
            repeat_count,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: ch as u16,
            control_key_state,
        })
    }

    fn key_vk_up_with_unicode(vk: u16, ch: char, control_key_state: u32) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: false,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: 0,
            unicode: ch as u16,
            control_key_state,
        })
    }

    fn key_vk_up_with_scan_unicode(
        vk: u16,
        scan: u16,
        ch: char,
        control_key_state: u32,
    ) -> WindowsInputRecord {
        WindowsInputRecord::Key(WindowsKeyRecord {
            key_down: false,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: scan,
            unicode: ch as u16,
            control_key_state,
        })
    }

    fn translate(
        records: impl IntoIterator<Item = WindowsInputRecord>,
    ) -> Vec<crate::protocol::ClientInputEvent> {
        let mut translator = WindowsInputTranslator::default();
        records
            .into_iter()
            .flat_map(|record| translator.translate(record))
            .collect()
    }

    fn win32_input_mode_encoded_raw_bytes(bytes: &[u8]) -> Vec<WindowsInputRecord> {
        bytes
            .iter()
            .flat_map(|byte| {
                format!("\x1b[0;0;{byte};1;0;1_")
                    .chars()
                    .map(key_char)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn win32_input_mode_encoded_key_bytes(bytes: &[u8]) -> Vec<WindowsInputRecord> {
        bytes
            .iter()
            .flat_map(|byte| {
                let vk = match *byte {
                    b'\x1b' => 0x1b,
                    b'\r' => 0x0d,
                    b'[' => 0xdb,
                    b'~' => 0xc0,
                    b'0'..=b'9' | b'A'..=b'Z' => u16::from(*byte),
                    b'a'..=b'z' => u16::from(byte.to_ascii_uppercase()),
                    _ => 0,
                };
                format!("\x1b[{vk};0;{byte};1;0;1_")
                    .chars()
                    .map(key_char)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn win32_input_mode_encoded_record(record: WindowsKeyRecord) -> Vec<WindowsInputRecord> {
        format!(
            "\x1b[{};{};{};{};{};{}_",
            record.virtual_key_code,
            record.virtual_scan_code,
            record.unicode,
            u16::from(record.key_down),
            record.control_key_state,
            record.repeat_count,
        )
        .chars()
        .map(key_char)
        .collect()
    }

    #[test]
    fn vti_bracketed_paste_records_emit_single_paste() {
        let records = "\x1b[200~alpha\rbravo\rcharlie\x1b[201~"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\rbravo\rcharlie".into(),
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_bracketed_paste_key_records_emit_single_paste() {
        let records = win32_input_mode_encoded_key_bytes(
            b"\x1b[200~About\ragent multiplexer that lives in your terminal.\x1b[201~",
        );

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "About\ragent multiplexer that lives in your terminal.".into(),
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_decoded_paste_handles_shift_repeats_and_releases() {
        let mut records = win32_input_mode_encoded_key_bytes(b"\x1b[200~");
        records.extend(win32_input_mode_encoded_record(WindowsKeyRecord {
            key_down: true,
            repeat_count: 2,
            virtual_key_code: 0x41,
            virtual_scan_code: 30,
            unicode: b'A'.into(),
            control_key_state: 0x0010,
        }));
        records.extend(win32_input_mode_encoded_record(WindowsKeyRecord {
            key_down: false,
            repeat_count: 1,
            virtual_key_code: 0x41,
            virtual_scan_code: 30,
            unicode: b'A'.into(),
            control_key_state: 0x0010,
        }));
        records.extend(win32_input_mode_encoded_key_bytes(b"\x1b[201~"));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste { text: "AA".into() }]
        );
    }

    #[test]
    fn vti_win32_input_mode_decoded_paste_flag_clears_after_raw_completion() {
        let mut records = win32_input_mode_encoded_key_bytes(b"\x1b[200~");
        records.extend("one\x1b[201~".chars().map(key_char));
        records.extend(
            "\x1b[200~x\x1b[65;30;97;1;0;1_y\x1b[201~"
                .chars()
                .map(key_char),
        );

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Paste { text: "one".into() },
                crate::protocol::ClientInputEvent::Paste {
                    text: "x\x1b[65;30;97;1;0;1_y".into(),
                },
            ]
        );
    }

    #[test]
    fn vti_incomplete_bracketed_paste_waits_for_terminator() {
        let mut translator = WindowsInputTranslator::default();
        for record in "\x1b[200~alpha\r".chars().map(key_char) {
            assert!(translator.translate(record).is_empty());
        }

        let mut events = Vec::new();
        for record in "bravo\x1b[201~".chars().map(key_char) {
            events.extend(translator.translate(record));
        }
        assert_eq!(
            events,
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\rbravo".into(),
            }]
        );
    }

    #[test]
    fn vti_ctrl_c_record_becomes_ctrl_c_key() {
        assert_eq!(
            translate([key_char('\u{3}')]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('c'),
                modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_ctrl_j_record_preserves_lf_control_key() {
        assert_eq!(
            translate([key_vk_with_unicode(0x4a, '\n', 0x0008)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('j'),
                modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_modifier_only_key_records_do_not_emit_terminal_input() {
        let modifier_records = [
            key_vk_with_utf16_mods(0x11, 0, 0x0008),
            key_vk_with_utf16_mods(0xa2, 0, 0x0008),
            key_vk_with_utf16_mods(0xa3, 0, 0x0004),
            key_vk_with_repeat(0x11, 3),
        ];

        assert!(translate(modifier_records).is_empty());
    }

    #[test]
    fn vti_win32_input_mode_modifier_only_key_records_do_not_emit_terminal_input() {
        let records = "\x1b[17;0;0;1;8;3_".chars().map(key_char);

        assert!(translate(records).is_empty());
    }

    #[test]
    fn vti_real_non_letter_control_records_are_preserved() {
        let cases = [
            (0x20, 0x00, ' '),
            (0xdc, 0x1c, '\\'),
            (0xdd, 0x1d, ']'),
            (0x36, 0x1e, '^'),
            (0xbd, 0x1f, '-'),
        ];

        for (vk, unicode, expected) in cases {
            assert_eq!(
                translate([key_vk_with_utf16_mods(vk, unicode, 0x0008)]),
                vec![crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char(expected),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                }],
                "vk={vk:#x} unicode={unicode:#x}"
            );
        }
    }

    #[test]
    fn vti_ctrl_bracket_record_flushes_to_escape_after_idle() {
        let mut translator = WindowsInputTranslator::default();
        assert!(translator
            .translate(key_vk_with_utf16_mods(0xdb, 0x1b, 0x0008))
            .is_empty());
        assert_eq!(
            translator.idle(),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_escape_key_record_flushes_to_escape_after_idle() {
        let mut translator = WindowsInputTranslator::default();
        assert!(translator.translate(key_vk(0x1b, 0)).is_empty());
        assert_eq!(
            translator.idle(),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_repeated_escape_record_stays_semantic() {
        assert_eq!(
            translate([key_vk_with_repeat(0x1b, 3)]),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Esc,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Esc,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Esc,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
            ]
        );
    }

    #[test]
    fn vti_modified_escape_remains_semantic() {
        assert_eq!(
            translate([key_vk_with_utf16_mods(0x1b, 0, 0x0010)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_key_release_does_not_emit_raw_text() {
        let records = "\x1b[0;0;97;0;0;1_".chars().map(key_char);
        assert!(translate(records).is_empty());
    }

    #[test]
    fn vti_enter_outside_paste_becomes_enter_key() {
        assert_eq!(
            translate([key_char('\r')]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_repeated_virtual_key_records_emit_repeats() {
        assert_eq!(
            translate([key_vk_with_repeat(0x08, 3)]),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Backspace,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Backspace,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Backspace,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
            ]
        );
    }

    #[test]
    fn vti_shift_enter_record_preserves_shift_modifier() {
        assert_eq!(
            translate([key_vk_with_unicode(0x0d, '\r', 0x0010)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_key_release_record_is_preserved() {
        assert_eq!(
            translate([key_vk_up_with_unicode(0x4a, 'j', 0x0008)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('j'),
                modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                kind: crate::protocol::ClientKeyKind::Release,
            }]
        );
    }

    #[test]
    fn vti_synthetic_shift_enter_record_preserves_shift_modifier() {
        assert_eq!(
            translate([key_vk_with_unicode(0, '\r', 0x0010)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_repeated_synthetic_shift_enter_emits_repeats() {
        assert_eq!(
            translate([key_vk_with_unicode_repeat(0, '\r', 0x0010, 3)]),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Repeat,
                },
            ]
        );
    }

    #[test]
    fn vti_synthetic_shift_enter_inside_paste_stays_in_paste_payload() {
        let mut records: Vec<_> = "\x1b[200~alpha".chars().map(key_char).collect();
        records.push(key_vk_with_unicode(0, '\r', 0x0010));
        records.extend("bravo\x1b[201~".chars().map(key_char));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\rbravo".into(),
            }]
        );
    }

    #[test]
    fn vti_vk_return_inside_paste_stays_in_paste_payload() {
        let mut records: Vec<_> = "\x1b[200~alpha".chars().map(key_char).collect();
        records.push(key_vk_with_unicode(0x0d, '\r', 0));
        records.extend("bravo\x1b[201~".chars().map(key_char));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\rbravo".into(),
            }]
        );
    }

    #[test]
    fn vti_vk_return_release_inside_paste_is_suppressed() {
        let mut records: Vec<_> = "\x1b[200~alpha".chars().map(key_char).collect();
        records.push(key_vk_up_with_unicode(0x0d, '\r', 0));
        records.extend("bravo\x1b[201~".chars().map(key_char));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alphabravo".into(),
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_shift_enter_preserves_shift_modifier() {
        let records =
            "\x1b[16;42;0;1;16;1_\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_\x1b[16;42;0;0;0;1_"
                .chars()
                .map(key_char);

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_win32_input_mode_plain_enter_stays_plain_enter() {
        let records = "\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_win32_input_mode_backspace_stays_backspace() {
        let records = "\x1b[8;14;8;1;0;1_\x1b[8;14;8;0;0;1_".chars().map(key_char);

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Backspace,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Backspace,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_win32_input_mode_ctrl_j_preserves_lf_control_key() {
        let records = "\x1b[74;36;10;1;8;1_\x1b[74;36;10;0;8;1_"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('j'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('j'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_alacritty_ctrl_j_return_lf_record_preserves_lf_control_key() {
        assert_eq!(
            translate([
                key_vk_with_scan_unicode(0x0d, 0x24, '\n', 0x0008),
                key_vk_up_with_scan_unicode(0x0d, 0x24, '\n', 0x0008),
            ]),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('j'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('j'),
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_ctrl_enter_return_lf_record_preserves_ctrl_enter() {
        assert_eq!(
            translate([
                key_vk_with_scan_unicode(0x0d, 0x1c, '\n', 0x0008),
                key_vk_up_with_scan_unicode(0x0d, 0x1c, '\n', 0x0008),
            ]),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_win32_input_mode_printable_key_becomes_char() {
        let records = "\x1b[65;30;97;1;0;1_\x1b[65;30;97;0;0;1_"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('a'),
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Char('a'),
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Release,
                },
            ]
        );
    }

    #[test]
    fn vti_win32_input_mode_sequence_inside_bracketed_paste_stays_payload() {
        let records = "\x1b[200~alpha\x1b[13;28;13;1;16;1_bravo\x1b[201~"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\x1b[13;28;13;1;16;1_bravo".into(),
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_raw_sequence_inside_bracketed_paste_stays_payload() {
        let records = "\x1b[200~alpha\x1b[0;0;97;1;0;1_bravo\x1b[201~"
            .chars()
            .map(key_char);

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "alpha\x1b[0;0;97;1;0;1_bravo".into(),
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_leaves_mouse_sequence_for_raw_parser() {
        let records = "\x1b[<0;3;4M".chars().map(key_char);

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Down(
                    crate::protocol::ClientMouseButton::Left,
                ),
                column: 2,
                row: 3,
                modifiers: 0,
            }]
        );
    }

    #[test]
    fn vti_ctrl_bracket_escape_starts_mouse_sequence() {
        let records = [key_vk_with_utf16_mods(0xdb, 0x1b, 0x0008)]
            .into_iter()
            .chain("[<35;48;26M".chars().map(key_char));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 47,
                row: 25,
                modifiers: 0,
            }]
        );
    }

    #[test]
    fn vti_escape_key_record_without_unicode_starts_mouse_sequence() {
        let records = [key_vk(0x1b, 0)]
            .into_iter()
            .chain("[<35;48;26M".chars().map(key_char));

        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 47,
                row: 25,
                modifiers: 0,
            }]
        );
    }

    #[test]
    fn vti_win32_input_mode_encoded_mouse_sequence() {
        assert_eq!(
            translate(win32_input_mode_encoded_raw_bytes(b"\x1b[<35;48;26M")),
            vec![crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 47,
                row: 25,
                modifiers: 0,
            }]
        );
    }

    #[test]
    fn vti_escape_arrow_sequence_becomes_arrow_key() {
        let records = "\x1b[A".chars().map(key_char);
        assert_eq!(
            translate(records),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Up,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_lone_escape_flushes_only_after_idle() {
        let mut translator = WindowsInputTranslator::default();
        assert!(translator.translate(key_char('\x1b')).is_empty());
        assert_eq!(
            translator.idle(),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_synthetic_shift_enter_after_lone_escape_stays_semantic() {
        let mut translator = WindowsInputTranslator::default();
        assert!(translator.translate(key_char('\x1b')).is_empty());
        assert_eq!(
            translator.translate(key_vk_with_unicode(0, '\r', 0x0010)),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Esc,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Enter,
                    modifiers: crossterm::event::KeyModifiers::SHIFT.bits(),
                    kind: crate::protocol::ClientKeyKind::Press,
                },
            ]
        );
    }

    #[test]
    fn vti_semantic_event_flushes_pending_raw_first() {
        let mut translator = WindowsInputTranslator::default();
        assert!(translator.translate(key_char('\x1b')).is_empty());
        assert_eq!(
            translator.translate(key_vk(0x26, 0)),
            vec![
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Esc,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
                crate::protocol::ClientInputEvent::Key {
                    code: crate::protocol::ClientKeyCode::Up,
                    modifiers: 0,
                    kind: crate::protocol::ClientKeyKind::Press,
                },
            ]
        );
    }

    #[test]
    fn vti_real_ctrl_c_record_stays_semantic() {
        assert_eq!(
            translate([key_vk_with_unicode(0x43, '\u{3}', 0x0008)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('c'),
                modifiers: crossterm::event::KeyModifiers::CONTROL.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_right_alt_special_key_preserves_alt_modifier() {
        assert_eq!(
            translate([key_vk(0x26, 0x0001)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Up,
                modifiers: crossterm::event::KeyModifiers::ALT.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_altgr_printable_key_is_not_terminal_alt_prefix() {
        assert_eq!(
            translate([key_vk_with_unicode(0x32, '@', 0x0001 | 0x0008)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('@'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_alt_code_unicode_on_alt_release_is_preserved() {
        assert_eq!(
            translate([key_vk_up_with_unicode(0x12, 'é', 0)]),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('é'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_semantic_surrogate_pair_preserves_emoji() {
        let mut translator = WindowsInputTranslator::default();
        let high = key_vk_with_utf16(0xe7, 0xd83d);
        let low = key_vk_with_utf16(0xe7, 0xde42);

        assert!(translator.translate(high).is_empty());
        assert_eq!(
            translator.translate(low),
            vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('🙂'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }]
        );
    }

    #[test]
    fn vti_surrogate_pair_paste_preserves_emoji() {
        let mut translator = WindowsInputTranslator::default();
        let records = [
            key_char('\x1b'),
            key_char('['),
            key_char('2'),
            key_char('0'),
            key_char('0'),
            key_char('~'),
            key_vk_with_utf16(0, 0xd83d),
            key_vk_with_utf16(0, 0xde42),
            key_char('\x1b'),
            key_char('['),
            key_char('2'),
            key_char('0'),
            key_char('1'),
            key_char('~'),
        ];

        let events = records
            .into_iter()
            .flat_map(|record| translator.translate(record))
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "🙂".into()
            }]
        );
    }

    #[test]
    fn vti_nonzero_vk_surrogate_pair_inside_paste_preserves_emoji() {
        let mut translator = WindowsInputTranslator::default();
        let high = key_vk_with_utf16(0xe7, 0xd83d);
        let low = key_vk_with_utf16(0xe7, 0xde42);
        let records = "\x1b[200~"
            .chars()
            .map(key_char)
            .chain([high, low])
            .chain("\x1b[201~".chars().map(key_char));

        let events = records
            .into_iter()
            .flat_map(|record| translator.translate(record))
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![crate::protocol::ClientInputEvent::Paste {
                text: "🙂".into()
            }]
        );
    }

    #[test]
    fn vti_mouse_press_release_records_are_preserved() {
        let events = translate([
            WindowsInputRecord::Mouse(WindowsMouseRecord {
                x: 3,
                y: 4,
                button_state: 0x0001,
                control_key_state: 0,
                event_flags: 0,
            }),
            WindowsInputRecord::Mouse(WindowsMouseRecord {
                x: 3,
                y: 4,
                button_state: 0,
                control_key_state: 0,
                event_flags: 0,
            }),
        ]);

        assert_eq!(
            events,
            vec![
                crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    column: 3,
                    row: 4,
                    modifiers: 0,
                },
                crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Up(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    column: 3,
                    row: 4,
                    modifiers: 0,
                },
            ]
        );
    }

    #[test]
    fn vti_horizontal_wheel_records_match_crossterm_direction() {
        let events = translate([
            WindowsInputRecord::Mouse(WindowsMouseRecord {
                x: 3,
                y: 4,
                button_state: 0xffff_0000,
                control_key_state: 0,
                event_flags: 0x0008,
            }),
            WindowsInputRecord::Mouse(WindowsMouseRecord {
                x: 3,
                y: 4,
                button_state: 0x0001_0000,
                control_key_state: 0,
                event_flags: 0x0008,
            }),
        ]);

        assert_eq!(
            events,
            vec![
                crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::ScrollLeft,
                    column: 3,
                    row: 4,
                    modifiers: 0,
                },
                crate::protocol::ClientInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::ScrollRight,
                    column: 3,
                    row: 4,
                    modifiers: 0,
                },
            ]
        );
    }

    #[test]
    fn vti_focus_records_are_preserved() {
        assert_eq!(
            translate([
                WindowsInputRecord::Focus(true),
                WindowsInputRecord::Focus(false)
            ]),
            vec![
                crate::protocol::ClientInputEvent::FocusGained,
                crate::protocol::ClientInputEvent::FocusLost,
            ]
        );
    }
}
