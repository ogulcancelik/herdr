#![allow(dead_code)]

#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    clippy::all,
    rustdoc::all
)]
pub mod bindings;

use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::os::raw::c_char;
use std::ptr;
use std::slice;

pub use bindings as ffi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(ffi::GhosttyResult);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ghostty error {}", self.0)
    }
}

impl std::error::Error for Error {}

trait GhosttyResultExt {
    fn into_result(self) -> Result<(), Error>;
}

impl GhosttyResultExt for ffi::GhosttyResult {
    fn into_result(self) -> Result<(), Error> {
        if self == ffi::GhosttyResult_GHOSTTY_SUCCESS {
            Ok(())
        } else {
            Err(Error(self))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    Clean,
    Partial,
    Full,
}

impl Dirty {
    fn from_raw(value: ffi::GhosttyRenderStateDirty) -> Self {
        match value {
            ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FALSE => Self::Clean,
            ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_PARTIAL => Self::Partial,
            ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FULL => Self::Full,
            _ => Self::Full,
        }
    }

    fn as_raw(self) -> ffi::GhosttyRenderStateDirty {
        match self {
            Self::Clean => ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FALSE,
            Self::Partial => ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_PARTIAL,
            Self::Full => ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FULL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    Gained,
    Lost,
}

impl FocusEvent {
    fn as_raw(self) -> ffi::GhosttyFocusEvent {
        match self {
            Self::Gained => ffi::GhosttyFocusEvent_GHOSTTY_FOCUS_GAINED,
            Self::Lost => ffi::GhosttyFocusEvent_GHOSTTY_FOCUS_LOST,
        }
    }
}

pub const MOD_SHIFT: u16 = ffi::GHOSTTY_MODS_SHIFT as u16;
pub const MOD_CTRL: u16 = ffi::GHOSTTY_MODS_CTRL as u16;
pub const MOD_ALT: u16 = ffi::GHOSTTY_MODS_ALT as u16;
pub const MOD_SUPER: u16 = ffi::GHOSTTY_MODS_SUPER as u16;

pub const KEY_ENTER: u32 = ffi::GhosttyKey_GHOSTTY_KEY_ENTER;
pub const KEY_UP: u32 = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP;
pub const KEY_DOWN: u32 = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN;
pub const KEY_LEFT: u32 = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT;
pub const KEY_RIGHT: u32 = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT;
pub const KEY_A: u32 = ffi::GhosttyKey_GHOSTTY_KEY_A;

pub const MOUSE_ACTION_PRESS: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS;
pub const MOUSE_ACTION_RELEASE: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE;
pub const MOUSE_ACTION_MOTION: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION;
pub const MOUSE_BUTTON_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT;
pub const MOUSE_BUTTON_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT;
pub const MOUSE_BUTTON_MIDDLE: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE;
pub const MOUSE_BUTTON_WHEEL_UP: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FOUR;
pub const MOUSE_BUTTON_WHEEL_DOWN: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FIVE;
pub const MOUSE_BUTTON_WHEEL_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SIX;
pub const MOUSE_BUTTON_WHEEL_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SEVEN;

pub const MODE_APPLICATION_CURSOR_KEYS: u16 = 1;
pub const MODE_FOCUS_EVENT: u16 = 1004;
pub const MODE_MOUSE_UTF8: u16 = 1005;
pub const MODE_MOUSE_SGR: u16 = 1006;
pub const MODE_MOUSE_ALTERNATE_SCROLL: u16 = 1007;
pub const MODE_BRACKETED_PASTE: u16 = 2004;
pub const MODE_SYNCHRONIZED_OUTPUT: u16 = 2026;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveScreen {
    Primary,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScrollbar {
    pub total: usize,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorViewport {
    pub x: u16,
    pub y: u16,
    pub wide_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<ffi::GhosttyColorRgb> for RgbColor {
    fn from(value: ffi::GhosttyColorRgb) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underlined: bool,
}

impl From<ffi::GhosttyStyle> for CellStyle {
    fn from(value: ffi::GhosttyStyle) -> Self {
        Self {
            bold: value.bold,
            italic: value.italic,
            faint: value.faint,
            blink: value.blink,
            inverse: value.inverse,
            invisible: value.invisible,
            strikethrough: value.strikethrough,
            overline: value.overline,
            underlined: value.underline != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColors {
    pub background: RgbColor,
    pub foreground: RgbColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWide {
    Narrow,
    Wide,
    SpacerTail,
    SpacerHead,
}

impl CellWide {
    fn from_raw(value: ffi::GhosttyCellWide) -> Self {
        match value {
            ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_NARROW => Self::Narrow,
            ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_WIDE => Self::Wide,
            ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_TAIL => Self::SpacerTail,
            ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_HEAD => Self::SpacerHead,
            _ => Self::Narrow,
        }
    }
}

struct WritePtyCallbackState {
    callback: Box<dyn FnMut(&[u8]) + Send>,
}

#[repr(C)]
struct GhosttyTerminalSelection {
    start: ffi::GhosttyPoint,
    end: ffi::GhosttyPoint,
    rectangle: bool,
}

unsafe extern "C" {
    fn ghostty_terminal_read_text(
        terminal: ffi::GhosttyTerminal_ptr,
        selection: GhosttyTerminalSelection,
        allocator: *const ffi::GhosttyAllocator,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> ffi::GhosttyResult;
}

unsafe extern "C" fn write_pty_trampoline(
    _terminal: ffi::GhosttyTerminal_ptr,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if userdata.is_null() {
        return;
    }
    let state = unsafe { &mut *(userdata.cast::<WritePtyCallbackState>()) };
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    (state.callback)(bytes);
}

pub fn encode_focus(event: FocusEvent) -> Result<Vec<u8>, Error> {
    let mut required = 0usize;
    // SAFETY: null buffer + out len is the documented way to query required size.
    let result =
        unsafe { ffi::ghostty_focus_encode(event.as_raw(), ptr::null_mut(), 0, &mut required) };
    if result != ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
        result.into_result()?;
    }

    let mut buffer = vec![0u8; required];
    // SAFETY: buffer is allocated for required size; function writes at most that many bytes.
    unsafe {
        ffi::ghostty_focus_encode(
            event.as_raw(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut required,
        )
        .into_result()?;
    }
    buffer.truncate(required);
    Ok(buffer)
}

pub struct Terminal {
    raw: ffi::GhosttyTerminal_ptr,
    write_pty_callback: Option<Box<WritePtyCallbackState>>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        let options = ffi::GhosttyTerminalOptions {
            cols,
            rows,
            max_scrollback,
        };
        // SAFETY: valid out pointer and options, null allocator means default allocator.
        unsafe {
            ffi::ghostty_terminal_new(ptr::null(), &mut raw, options).into_result()?;
        }
        Ok(Self {
            raw,
            write_pty_callback: None,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: self.raw is a live terminal handle for self's lifetime.
        unsafe {
            ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len());
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        // SAFETY: self.raw is valid and sizes are plain values.
        unsafe { ffi::ghostty_terminal_resize(self.raw, cols, rows, 0, 0).into_result() }
    }

    pub fn set_write_pty_callback<F>(&mut self, callback: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        let mut state = Box::new(WritePtyCallbackState {
            callback: Box::new(callback),
        });
        let userdata = (&mut *state as *mut WritePtyCallbackState).cast::<c_void>();
        unsafe {
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_USERDATA,
                userdata.cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_WRITE_PTY,
                (write_pty_trampoline as *const ()).cast(),
            )
            .into_result()?;
        }
        self.write_pty_callback = Some(state);
        Ok(())
    }

    pub fn mode_get(&self, mode: u16) -> Result<bool, Error> {
        let mut out = false;
        unsafe { ffi::ghostty_terminal_mode_get(self.raw, mode, &mut out).into_result()? };
        Ok(out)
    }

    pub fn mode_set(&mut self, mode: u16, value: bool) -> Result<(), Error> {
        unsafe { ffi::ghostty_terminal_mode_set(self.raw, mode, value).into_result() }
    }

    pub fn kitty_keyboard_flags(&self) -> Result<u8, Error> {
        let mut out = 0u8;
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS,
                (&mut out as *mut u8).cast(),
            )
            .into_result()?;
        }
        Ok(out)
    }

    pub fn mouse_tracking_enabled(&self) -> Result<bool, Error> {
        let mut out = false;
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING,
                (&mut out as *mut bool).cast(),
            )
            .into_result()?;
        }
        Ok(out)
    }

    pub fn active_screen(&self) -> Result<ActiveScreen, Error> {
        let mut out = ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_PRIMARY;
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                (&mut out as *mut ffi::GhosttyTerminalScreen).cast(),
            )
            .into_result()?;
        }
        Ok(match out {
            ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_PRIMARY => ActiveScreen::Primary,
            ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_ALTERNATE => ActiveScreen::Alternate,
            _ => ActiveScreen::Primary,
        })
    }

    pub fn total_rows(&self) -> Result<usize, Error> {
        self.get_usize(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_TOTAL_ROWS)
    }

    pub fn scrollback_rows(&self) -> Result<usize, Error> {
        self.get_usize(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS)
    }

    pub fn scrollbar(&self) -> Result<TerminalScrollbar, Error> {
        let mut out = ffi::GhosttyTerminalScrollbar::default();
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBAR,
                (&mut out as *mut ffi::GhosttyTerminalScrollbar).cast(),
            )
            .into_result()?;
        }
        Ok(TerminalScrollbar {
            total: out.total as usize,
            offset: out.offset as usize,
            len: out.len as usize,
        })
    }

    pub fn screen_graphemes(&self, x: u16, y: u32) -> Result<Vec<u32>, Error> {
        let point = ffi::GhosttyPoint {
            tag: ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_SCREEN,
            value: ffi::GhosttyPointValue {
                coordinate: ffi::GhosttyPointCoordinate { x, y },
            },
        };
        let mut grid_ref = ffi::GhosttyGridRef {
            size: mem::size_of::<ffi::GhosttyGridRef>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_terminal_grid_ref(self.raw, point, &mut grid_ref).into_result()?;
        }
        let mut required = 0usize;
        let result = unsafe {
            ffi::ghostty_grid_ref_graphemes(&grid_ref, ptr::null_mut(), 0, &mut required)
        };
        if result != ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
            result.into_result()?;
        }
        let mut buffer = vec![0u32; required];
        if required == 0 {
            return Ok(buffer);
        }
        unsafe {
            ffi::ghostty_grid_ref_graphemes(
                &grid_ref,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut required,
            )
            .into_result()?;
        }
        buffer.truncate(required);
        Ok(buffer)
    }

    pub fn read_text_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let selection = GhosttyTerminalSelection {
            start: ghostty_viewport_point(start.0, start.1),
            end: ghostty_viewport_point(end.0, end.1),
            rectangle,
        };
        let mut out_ptr = ptr::null_mut();
        let mut out_len = 0usize;
        unsafe {
            ghostty_terminal_read_text(
                self.raw,
                selection,
                ptr::null(),
                &mut out_ptr,
                &mut out_len,
            )
            .into_result()?;
        }

        let text = if out_len == 0 {
            String::new()
        } else {
            let bytes = unsafe { slice::from_raw_parts(out_ptr.cast_const(), out_len) };
            String::from_utf8_lossy(bytes).into_owned()
        };

        if !out_ptr.is_null() {
            unsafe {
                ffi::ghostty_free(ptr::null(), out_ptr, out_len);
            }
        }

        Ok(text)
    }

    pub fn scroll_viewport_bottom(&mut self) {
        let viewport = ffi::GhosttyTerminalScrollViewport {
            tag: ffi::GhosttyTerminalScrollViewportTag_GHOSTTY_SCROLL_VIEWPORT_BOTTOM,
            value: ffi::GhosttyTerminalScrollViewportValue::default(),
        };
        // SAFETY: self.raw is valid and viewport value matches the tag.
        unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.raw, viewport);
        }
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        let viewport = ffi::GhosttyTerminalScrollViewport {
            tag: ffi::GhosttyTerminalScrollViewportTag_GHOSTTY_SCROLL_VIEWPORT_DELTA,
            value: ffi::GhosttyTerminalScrollViewportValue { delta },
        };
        // SAFETY: self.raw is valid and viewport value matches the tag.
        unsafe {
            ffi::ghostty_terminal_scroll_viewport(self.raw, viewport);
        }
    }

    pub fn cols(&self) -> Result<u16, Error> {
        self.get_u16(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_COLS)
    }

    pub fn rows(&self) -> Result<u16, Error> {
        self.get_u16(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_ROWS)
    }

    fn get_u16(&self, data: ffi::GhosttyTerminalData) -> Result<u16, Error> {
        let mut out = 0u16;
        // SAFETY: out points to a u16 matching the requested terminal data type.
        unsafe {
            ffi::ghostty_terminal_get(self.raw, data, (&mut out as *mut u16).cast())
                .into_result()?;
        }
        Ok(out)
    }

    fn get_usize(&self, data: ffi::GhosttyTerminalData) -> Result<usize, Error> {
        let mut out = 0usize;
        unsafe {
            ffi::ghostty_terminal_get(self.raw, data, (&mut out as *mut usize).cast())
                .into_result()?;
        }
        Ok(out)
    }

    fn raw(&self) -> ffi::GhosttyTerminal_ptr {
        self.raw
    }
}

// SAFETY: these opaque handles are only used behind external synchronization in pane runtime.
unsafe impl Send for Terminal {}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: freeing a null or live handle is allowed by the C API.
        unsafe {
            ffi::ghostty_terminal_free(self.raw);
        }
    }
}

fn ghostty_viewport_point(x: u16, y: u32) -> ffi::GhosttyPoint {
    ffi::GhosttyPoint {
        tag: ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_VIEWPORT,
        value: ffi::GhosttyPointValue {
            coordinate: ffi::GhosttyPointCoordinate { x, y },
        },
    }
}

pub struct RenderState {
    raw: ffi::GhosttyRenderState_ptr,
}

impl RenderState {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        // SAFETY: valid out pointer and null allocator use default allocator.
        unsafe {
            ffi::ghostty_render_state_new(ptr::null(), &mut raw).into_result()?;
        }
        Ok(Self { raw })
    }

    pub fn update(&mut self, terminal: &Terminal) -> Result<(), Error> {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { ffi::ghostty_render_state_update(self.raw, terminal.raw()).into_result() }
    }

    pub fn cols(&self) -> Result<u16, Error> {
        self.get_u16(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_COLS)
    }

    pub fn rows(&self) -> Result<u16, Error> {
        self.get_u16(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_ROWS)
    }

    pub fn dirty(&self) -> Result<Dirty, Error> {
        let mut out = ffi::GhosttyRenderStateDirty_GHOSTTY_RENDER_STATE_DIRTY_FALSE;
        // SAFETY: out points to the matching enum storage for the requested data kind.
        unsafe {
            ffi::ghostty_render_state_get(
                self.raw,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_DIRTY,
                (&mut out as *mut ffi::GhosttyRenderStateDirty).cast(),
            )
            .into_result()?;
        }
        Ok(Dirty::from_raw(out))
    }

    pub fn cursor_visible(&self) -> Result<bool, Error> {
        self.get_bool(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VISIBLE)
    }

    pub fn cursor_viewport(&self) -> Result<Option<CursorViewport>, Error> {
        if !self.get_bool(
            ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE,
        )? {
            return Ok(None);
        }
        Ok(Some(CursorViewport {
            x: self
                .get_u16(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_X)?,
            y: self
                .get_u16(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_Y)?,
            wide_tail: self.get_bool(
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL,
            )?,
        }))
    }

    pub fn colors(&self) -> Result<RenderColors, Error> {
        let mut colors = ffi::GhosttyRenderStateColors {
            size: mem::size_of::<ffi::GhosttyRenderStateColors>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_render_state_colors_get(self.raw, &mut colors).into_result()?;
        }
        Ok(RenderColors {
            background: colors.background.into(),
            foreground: colors.foreground.into(),
        })
    }

    pub fn set_dirty(&mut self, dirty: Dirty) -> Result<(), Error> {
        let value = dirty.as_raw();
        // SAFETY: value pointer matches the expected option type.
        unsafe {
            ffi::ghostty_render_state_set(
                self.raw,
                ffi::GhosttyRenderStateOption_GHOSTTY_RENDER_STATE_OPTION_DIRTY,
                (&value as *const ffi::GhosttyRenderStateDirty).cast(),
            )
            .into_result()
        }
    }

    pub fn populate_row_iterator<'a>(
        &'a self,
        iterator: &'a mut RowIterator,
    ) -> Result<RowIter<'a>, Error> {
        // SAFETY: iterator raw handle is valid and will not outlive self.
        unsafe {
            ffi::ghostty_render_state_get(
                self.raw,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                (&mut iterator.raw as *mut ffi::GhosttyRenderStateRowIterator_ptr).cast(),
            )
            .into_result()?;
        }
        Ok(RowIter {
            iterator,
            _state: PhantomData,
        })
    }

    fn get_u16(&self, data: ffi::GhosttyRenderStateData) -> Result<u16, Error> {
        let mut out = 0u16;
        // SAFETY: out points to a u16 matching the requested render-state data type.
        unsafe {
            ffi::ghostty_render_state_get(self.raw, data, (&mut out as *mut u16).cast())
                .into_result()?;
        }
        Ok(out)
    }

    fn get_bool(&self, data: ffi::GhosttyRenderStateData) -> Result<bool, Error> {
        let mut out = false;
        unsafe {
            ffi::ghostty_render_state_get(self.raw, data, (&mut out as *mut bool).cast())
                .into_result()?;
        }
        Ok(out)
    }
}

// SAFETY: these opaque handles are only used behind external synchronization in pane runtime.
unsafe impl Send for RenderState {}

impl Drop for RenderState {
    fn drop(&mut self) {
        // SAFETY: freeing a null or live handle is allowed by the C API.
        unsafe {
            ffi::ghostty_render_state_free(self.raw);
        }
    }
}

pub struct KeyEvent {
    raw: ffi::GhosttyKeyEvent_ptr,
}

impl KeyEvent {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        unsafe { ffi::ghostty_key_event_new(ptr::null(), &mut raw).into_result()? };
        Ok(Self { raw })
    }

    pub fn set_action(&mut self, action: ffi::GhosttyKeyAction) {
        unsafe { ffi::ghostty_key_event_set_action(self.raw, action) }
    }

    pub fn set_key(&mut self, key: u32) {
        unsafe { ffi::ghostty_key_event_set_key(self.raw, key) }
    }

    pub fn set_mods(&mut self, mods: u16) {
        unsafe { ffi::ghostty_key_event_set_mods(self.raw, mods) }
    }

    pub fn set_utf8(&mut self, text: &str) {
        unsafe {
            ffi::ghostty_key_event_set_utf8(self.raw, text.as_ptr().cast::<c_char>(), text.len())
        }
    }

    pub fn set_unshifted_codepoint(&mut self, codepoint: u32) {
        unsafe { ffi::ghostty_key_event_set_unshifted_codepoint(self.raw, codepoint) }
    }
}

impl Drop for KeyEvent {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_key_event_free(self.raw) }
    }
}

pub struct KeyEncoder {
    raw: ffi::GhosttyKeyEncoder_ptr,
}

impl KeyEncoder {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        unsafe { ffi::ghostty_key_encoder_new(ptr::null(), &mut raw).into_result()? };
        Ok(Self { raw })
    }

    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        unsafe { ffi::ghostty_key_encoder_setopt_from_terminal(self.raw, terminal.raw()) }
    }

    pub fn encode(&mut self, event: &KeyEvent) -> Result<Vec<u8>, Error> {
        encode_with_retry(|buf, len, out_len| unsafe {
            ffi::ghostty_key_encoder_encode(self.raw, event.raw, buf, len, out_len)
        })
    }
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_key_encoder_free(self.raw) }
    }
}

pub struct MouseEvent {
    raw: ffi::GhosttyMouseEvent_ptr,
}

impl MouseEvent {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        unsafe { ffi::ghostty_mouse_event_new(ptr::null(), &mut raw).into_result()? };
        Ok(Self { raw })
    }

    pub fn set_action(&mut self, action: ffi::GhosttyMouseAction) {
        unsafe { ffi::ghostty_mouse_event_set_action(self.raw, action) }
    }

    pub fn set_button(&mut self, button: ffi::GhosttyMouseButton) {
        unsafe { ffi::ghostty_mouse_event_set_button(self.raw, button) }
    }

    pub fn clear_button(&mut self) {
        unsafe { ffi::ghostty_mouse_event_clear_button(self.raw) }
    }

    pub fn set_mods(&mut self, mods: u16) {
        unsafe { ffi::ghostty_mouse_event_set_mods(self.raw, mods) }
    }

    pub fn set_position(&mut self, x: f32, y: f32) {
        unsafe {
            ffi::ghostty_mouse_event_set_position(self.raw, ffi::GhosttyMousePosition { x, y })
        }
    }
}

impl Drop for MouseEvent {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_mouse_event_free(self.raw) }
    }
}

pub struct MouseEncoder {
    raw: ffi::GhosttyMouseEncoder_ptr,
}

impl MouseEncoder {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        unsafe { ffi::ghostty_mouse_encoder_new(ptr::null(), &mut raw).into_result()? };
        Ok(Self { raw })
    }

    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        unsafe { ffi::ghostty_mouse_encoder_setopt_from_terminal(self.raw, terminal.raw()) }
    }

    pub fn set_size(
        &mut self,
        screen_width: u32,
        screen_height: u32,
        cell_width: u32,
        cell_height: u32,
    ) {
        let size = ffi::GhosttyMouseEncoderSize {
            size: std::mem::size_of::<ffi::GhosttyMouseEncoderSize>(),
            screen_width,
            screen_height,
            cell_width,
            cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        unsafe {
            ffi::ghostty_mouse_encoder_setopt(
                self.raw,
                ffi::GhosttyMouseEncoderOption_GHOSTTY_MOUSE_ENCODER_OPT_SIZE,
                (&size as *const ffi::GhosttyMouseEncoderSize).cast(),
            )
        }
    }

    pub fn encode(&mut self, event: &MouseEvent) -> Result<Vec<u8>, Error> {
        encode_with_retry(|buf, len, out_len| unsafe {
            ffi::ghostty_mouse_encoder_encode(self.raw, event.raw, buf, len, out_len)
        })
    }
}

impl Drop for MouseEncoder {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_mouse_encoder_free(self.raw) }
    }
}

fn encode_with_retry(
    mut encode: impl FnMut(*mut c_char, usize, *mut usize) -> ffi::GhosttyResult,
) -> Result<Vec<u8>, Error> {
    let mut required = 0usize;
    let result = encode(ptr::null_mut(), 0, &mut required);
    if result != ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
        result.into_result()?;
    }
    let mut buffer = vec![0u8; required.max(16)];
    let mut written = 0usize;
    encode(
        buffer.as_mut_ptr().cast::<c_char>(),
        buffer.len(),
        &mut written,
    )
    .into_result()?;
    buffer.truncate(written);
    Ok(buffer)
}

pub struct RowIterator {
    raw: ffi::GhosttyRenderStateRowIterator_ptr,
}

impl RowIterator {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        // SAFETY: valid out pointer and null allocator use default allocator.
        unsafe {
            ffi::ghostty_render_state_row_iterator_new(ptr::null(), &mut raw).into_result()?;
        }
        Ok(Self { raw })
    }
}

// SAFETY: these opaque handles are only used behind external synchronization in pane runtime.
unsafe impl Send for RowIterator {}

impl Drop for RowIterator {
    fn drop(&mut self) {
        // SAFETY: freeing a null or live handle is allowed by the C API.
        unsafe {
            ffi::ghostty_render_state_row_iterator_free(self.raw);
        }
    }
}

pub struct RowIter<'a> {
    iterator: &'a mut RowIterator,
    _state: PhantomData<&'a RenderState>,
}

impl<'a> RowIter<'a> {
    pub fn next(&mut self) -> bool {
        // SAFETY: iterator handle is valid while self is alive.
        unsafe { ffi::ghostty_render_state_row_iterator_next(self.iterator.raw) }
    }

    pub fn dirty(&self) -> Result<bool, Error> {
        let mut dirty = false;
        // SAFETY: dirty output matches requested row data type.
        unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.raw,
                ffi::GhosttyRenderStateRowData_GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY,
                (&mut dirty as *mut bool).cast(),
            )
            .into_result()?;
        }
        Ok(dirty)
    }

    pub fn populate_cells<'b>(
        &'b mut self,
        cells: &'b mut RowCells,
    ) -> Result<RowCellIter<'b>, Error> {
        // SAFETY: cells raw handle is valid and will not outlive the current row borrow.
        unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.raw,
                ffi::GhosttyRenderStateRowData_GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                (&mut cells.raw as *mut ffi::GhosttyRenderStateRowCells_ptr).cast(),
            )
            .into_result()?;
        }
        Ok(RowCellIter { cells })
    }
}

pub struct RowCells {
    raw: ffi::GhosttyRenderStateRowCells_ptr,
}

impl RowCells {
    pub fn new() -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        // SAFETY: valid out pointer and null allocator use default allocator.
        unsafe {
            ffi::ghostty_render_state_row_cells_new(ptr::null(), &mut raw).into_result()?;
        }
        Ok(Self { raw })
    }
}

// SAFETY: these opaque handles are only used behind external synchronization in pane runtime.
unsafe impl Send for RowCells {}

impl Drop for RowCells {
    fn drop(&mut self) {
        // SAFETY: freeing a null or live handle is allowed by the C API.
        unsafe {
            ffi::ghostty_render_state_row_cells_free(self.raw);
        }
    }
}

pub struct RowCellIter<'a> {
    cells: &'a mut RowCells,
}

impl<'a> RowCellIter<'a> {
    pub fn next(&mut self) -> bool {
        // SAFETY: cells handle is valid while self is alive.
        unsafe { ffi::ghostty_render_state_row_cells_next(self.cells.raw) }
    }

    pub fn select(&mut self, x: u16) -> Result<(), Error> {
        unsafe { ffi::ghostty_render_state_row_cells_select(self.cells.raw, x).into_result() }
    }

    fn raw_cell(&self) -> Result<ffi::GhosttyCell, Error> {
        let mut raw = ffi::GhosttyCell::default();
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
                (&mut raw as *mut ffi::GhosttyCell).cast(),
            )
            .into_result()?;
        }
        Ok(raw)
    }

    pub fn wide(&self) -> Result<CellWide, Error> {
        let raw = self.raw_cell()?;
        let mut wide = ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_NARROW;
        unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_WIDE,
                (&mut wide as *mut ffi::GhosttyCellWide).cast(),
            )
            .into_result()?;
        }
        Ok(CellWide::from_raw(wide))
    }

    pub fn style(&self) -> Result<CellStyle, Error> {
        let mut style = ffi::GhosttyStyle {
            size: mem::size_of::<ffi::GhosttyStyle>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
                (&mut style as *mut ffi::GhosttyStyle).cast(),
            )
            .into_result()?;
        }
        Ok(style.into())
    }

    pub fn fg_color(&self) -> Result<Option<RgbColor>, Error> {
        let mut color = ffi::GhosttyColorRgb::default();
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR,
                (&mut color as *mut ffi::GhosttyColorRgb).cast(),
            )
        };
        match result {
            ffi::GhosttyResult_GHOSTTY_SUCCESS => Ok(Some(color.into())),
            ffi::GhosttyResult_GHOSTTY_INVALID_VALUE => Ok(None),
            other => Err(Error(other)),
        }
    }

    pub fn bg_color(&self) -> Result<Option<RgbColor>, Error> {
        let mut color = ffi::GhosttyColorRgb::default();
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR,
                (&mut color as *mut ffi::GhosttyColorRgb).cast(),
            )
        };
        match result {
            ffi::GhosttyResult_GHOSTTY_SUCCESS => Ok(Some(color.into())),
            ffi::GhosttyResult_GHOSTTY_INVALID_VALUE => Ok(None),
            other => Err(Error(other)),
        }
    }

    pub fn grapheme_len(&self) -> Result<u32, Error> {
        let mut len = 0u32;
        // SAFETY: len output matches requested cell data type.
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                (&mut len as *mut u32).cast(),
            )
            .into_result()?;
        }
        Ok(len)
    }

    pub fn graphemes(&self) -> Result<Vec<u32>, Error> {
        let len = self.grapheme_len()? as usize;
        let mut out = vec![0u32; len];
        if len == 0 {
            return Ok(out);
        }
        // SAFETY: out buffer is allocated for the grapheme count returned by the API.
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                out.as_mut_ptr().cast::<c_void>(),
            )
            .into_result()?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_numbered_lines(terminal: &mut Terminal, count: usize) {
        for i in 0..count {
            terminal.write(format!("{i:06}\r\n").as_bytes());
        }
    }

    fn write_padded_lines(terminal: &mut Terminal, count: usize, width: usize) {
        let line = format!("{}\r\n", "x".repeat(width));
        for _ in 0..count {
            terminal.write(line.as_bytes());
        }
    }

    #[test]
    fn focus_encoding_matches_expected_sequences() {
        assert_eq!(encode_focus(FocusEvent::Gained).unwrap(), b"\x1b[I");
        assert_eq!(encode_focus(FocusEvent::Lost).unwrap(), b"\x1b[O");
    }

    #[test]
    fn write_pty_callback_receives_terminal_query_responses() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        let responses = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let sink = responses.clone();
        terminal
            .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
            .unwrap();

        terminal.write(b"\x1b[6n");

        let output = responses.lock().unwrap().clone();
        assert!(!output.is_empty());
        assert!(String::from_utf8_lossy(&output).contains("R"));
    }

    #[test]
    fn key_and_mouse_encoders_follow_terminal_state() {
        let mut terminal = Terminal::new(80, 24, 0).unwrap();
        terminal.mode_set(1, true).unwrap();
        terminal.write(b"\x1b[>1u\x1b[?1000h\x1b[?1006h");

        assert!(terminal.mode_get(1).unwrap());
        assert_eq!(terminal.kitty_keyboard_flags().unwrap(), 1);
        assert!(terminal.mouse_tracking_enabled().unwrap());

        let mut key_encoder = KeyEncoder::new().unwrap();
        key_encoder.set_from_terminal(&terminal);
        let mut key_event = KeyEvent::new().unwrap();
        key_event.set_action(ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS);
        key_event.set_key(KEY_A);
        key_event.set_mods(MOD_CTRL | MOD_SHIFT);
        key_event.set_utf8("A");
        key_event.set_unshifted_codepoint('a' as u32);
        let encoded_key = key_encoder.encode(&key_event).unwrap();
        assert_eq!(encoded_key, b"\x1b[97;6u");

        let mut mouse_encoder = MouseEncoder::new().unwrap();
        mouse_encoder.set_from_terminal(&terminal);
        mouse_encoder.set_size(80, 24, 1, 1);
        let mut mouse_event = MouseEvent::new().unwrap();
        mouse_event.set_action(ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS);
        mouse_event.set_button(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT);
        mouse_event.set_position(0.0, 0.0);
        let encoded_mouse = mouse_encoder.encode(&mouse_event).unwrap();
        assert_eq!(encoded_mouse, b"\x1b[<0;1;1M");
    }

    #[test]
    fn terminal_read_text_viewport_unwraps_soft_wrapped_selection() {
        let mut terminal = Terminal::new(5, 3, 0).unwrap();
        terminal.write("1ABCD2EFGH3IJKL".as_bytes());

        let text = terminal.read_text_viewport((0, 1), (2, 2), false).unwrap();
        assert_eq!(text, "2EFGH3IJ");
    }

    #[test]
    fn terminal_read_text_viewport_handles_wide_chars() {
        let mut terminal = Terminal::new(5, 3, 0).unwrap();
        terminal.write("1A⚡".as_bytes());

        let full = terminal.read_text_viewport((0, 0), (3, 0), false).unwrap();
        assert_eq!(full, "1A⚡");

        let through_wide_head = terminal.read_text_viewport((0, 0), (2, 0), false).unwrap();
        assert_eq!(through_wide_head, "1A⚡");

        let wide_only = terminal.read_text_viewport((3, 0), (3, 0), false).unwrap();
        assert_eq!(wide_only, "⚡");
    }

    #[test]
    fn zero_max_scrollback_disables_history() {
        let mut terminal = Terminal::new(80, 3, 0).unwrap();
        write_numbered_lines(&mut terminal, 3000);
        assert_eq!(terminal.scrollback_rows().unwrap(), 0);
    }

    #[test]
    fn max_scrollback_limit_bytes_retains_more_history_for_larger_limits() {
        let mut small = Terminal::new(80, 3, 1_000_000).unwrap();
        let mut large = Terminal::new(80, 3, 10_000_000).unwrap();

        write_padded_lines(&mut small, 20_000, 70);
        write_padded_lines(&mut large, 20_000, 70);

        let small_scrollback = small.scrollback_rows().unwrap();
        let large_scrollback = large.scrollback_rows().unwrap();

        assert!(
            large_scrollback > small_scrollback,
            "expected larger byte limit to retain more history, got small={small_scrollback}, large={large_scrollback}"
        );
    }

    #[test]
    fn large_negative_scroll_delta_reaches_top_of_scrollback() {
        let mut terminal = Terminal::new(80, 3, 1_000_000).unwrap();
        write_numbered_lines(&mut terminal, 1000);

        let before = terminal.scrollbar().unwrap();
        assert!(before.total > before.len);

        terminal.scroll_viewport_bottom();
        terminal.scroll_viewport_delta(-10_000);

        let after = terminal.scrollbar().unwrap();
        assert_eq!(after.offset, 0);
        assert_eq!(after.len, before.len);
    }

    #[test]
    fn terminal_and_render_state_smoke_test() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        assert_eq!(terminal.cols().unwrap(), 8);
        assert_eq!(terminal.rows().unwrap(), 3);

        terminal.write(b"hello\r\nworld");

        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();
        assert_eq!(render_state.cols().unwrap(), 8);
        assert_eq!(render_state.rows().unwrap(), 3);
        assert_ne!(render_state.dirty().unwrap(), Dirty::Clean);

        let mut row_iterator = RowIterator::new().unwrap();
        let mut row_iter = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        let mut row_cells = RowCells::new().unwrap();

        let mut found_hello = false;
        let mut found_world = false;
        let mut row_index = 0usize;
        while row_iter.next() {
            let _ = row_iter.dirty().unwrap();
            let mut cells = row_iter.populate_cells(&mut row_cells).unwrap();
            let mut line = String::new();
            while cells.next() {
                let graphemes = cells.graphemes().unwrap();
                if let Some(codepoint) = graphemes.first().copied() {
                    if let Some(ch) = char::from_u32(codepoint) {
                        line.push(ch);
                    }
                } else {
                    line.push(' ');
                }
            }
            let trimmed = line.trim_end().to_string();
            if row_index == 0 {
                found_hello = trimmed.starts_with("hello");
            }
            if row_index == 1 {
                found_world = trimmed.starts_with("world");
            }
            row_index += 1;
        }

        assert!(found_hello);
        assert!(found_world);

        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
    }
}
