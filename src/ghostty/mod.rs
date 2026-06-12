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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem;
use std::ops::RangeInclusive;
use std::os::raw::c_char;
use std::ptr;
use std::slice;
use std::sync::{Once, OnceLock};

pub use bindings as ffi;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(ffi::GhosttyResult);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatterFormat {
    Plain,
    Vt,
}

impl FormatterFormat {
    fn as_raw(self) -> ffi::GhosttyFormatterFormat {
        match self {
            Self::Plain => ffi::GhosttyFormatterFormat_GHOSTTY_FORMATTER_FORMAT_PLAIN,
            Self::Vt => ffi::GhosttyFormatterFormat_GHOSTTY_FORMATTER_FORMAT_VT,
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSelection {
    pub start_x: u16,
    pub end_x: u16,
}

impl RowSelection {
    pub fn range(self) -> RangeInclusive<u16> {
        self.start_x..=self.end_x
    }
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
pub const MOUSE_FORMAT_SGR: ffi::GhosttyMouseFormat =
    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR;

pub const MODE_APPLICATION_CURSOR_KEYS: u16 = 1;
pub const MODE_FOCUS_EVENT: u16 = 1004;
pub const MODE_MOUSE_UTF8: u16 = 1005;
pub const MODE_MOUSE_SGR: u16 = 1006;
pub const MODE_MOUSE_ALTERNATE_SCROLL: u16 = 1007;
pub const MODE_MOUSE_SGR_PIXELS: u16 = 1016;
pub const MODE_BRACKETED_PASTE: u16 = 2004;
pub const MODE_SYNCHRONIZED_OUTPUT: u16 = 2026;

const KITTY_IMAGE_STORAGE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const APC_MAX_BYTES: usize = 16 * 1024 * 1024;
const APC_MAX_BYTES_KITTY: usize = 16 * 1024 * 1024;
// Kitty image fingerprints are used as a display cache key, not a
// cryptographic identity. Sampling keeps redraws cheap for multi-megabyte
// images while still distinguishing normal screenshots/photos/diagrams.
const KITTY_FINGERPRINT_SAMPLE_BYTES: usize = 4096;
pub(crate) const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;
// The vendored C headers expose these placement fields, but the checked-in
// generated bindings predate the names. Keep the explicit values aligned with
// vendor/libghostty-vt/include/ghostty/vt/kitty_graphics.h.
const KITTY_PLACEMENT_DATA_IS_VIRTUAL: ffi::GhosttyKittyGraphicsPlacementData = 3;
const KITTY_PLACEMENT_DATA_COLUMNS: ffi::GhosttyKittyGraphicsPlacementData = 10;
const KITTY_PLACEMENT_DATA_ROWS: ffi::GhosttyKittyGraphicsPlacementData = 11;

static INSTALL_PNG_DECODER: Once = Once::new();
static KITTY_PLACEHOLDER_DIACRITICS: OnceLock<HashMap<u32, u32>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum KittyImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub z: i32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
    pub data: Vec<u8>,
    pub render: KittyPlacementRenderInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyImageDescriptor {
    pub image_id: u32,
    pub placement_id: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlacementRenderInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub viewport_col: i32,
    pub viewport_row: i32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyVirtualPlacementSpec {
    image_id: u32,
    placement_id: u32,
    columns: u32,
    rows: u32,
    z: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyVirtualCell {
    x: u16,
    y: u16,
    image_id_low: u32,
    image_id_high: Option<u32>,
    placement_id: Option<u32>,
    row: Option<u32>,
    col: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyVirtualRun {
    x: u16,
    y: u16,
    image_id_low: u32,
    image_id_high: Option<u32>,
    placement_id: Option<u32>,
    row: u32,
    col: u32,
    width: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyVirtualPlacementGeometry {
    x_offset: u32,
    y_offset: u32,
    render: KittyPlacementRenderInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorVisualStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

impl CursorVisualStyle {
    fn from_raw(value: ffi::GhosttyRenderStateCursorVisualStyle) -> Self {
        match value {
            ffi::GhosttyRenderStateCursorVisualStyle_GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK => {
                Self::Block
            }
            ffi::GhosttyRenderStateCursorVisualStyle_GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE => {
                Self::Underline
            }
            ffi::GhosttyRenderStateCursorVisualStyle_GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW => {
                Self::BlockHollow
            }
            _ => Self::Bar,
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellColor {
    Palette(u8),
    Rgb(RgbColor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg_color: Option<CellColor>,
    pub bg_color: Option<CellColor>,
    pub underline_color: Option<CellColor>,
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
            fg_color: cell_color_from_style_color(value.fg_color),
            bg_color: cell_color_from_style_color(value.bg_color),
            underline_color: cell_color_from_style_color(value.underline_color),
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

fn cell_color_from_style_color(color: ffi::GhosttyStyleColor) -> Option<CellColor> {
    match color.tag {
        ffi::GhosttyStyleColorTag_GHOSTTY_STYLE_COLOR_PALETTE => {
            // SAFETY: Ghostty's tagged union stores `palette` when the tag is PALETTE.
            Some(CellColor::Palette(unsafe { color.value.palette }))
        }
        ffi::GhosttyStyleColorTag_GHOSTTY_STYLE_COLOR_RGB => {
            // SAFETY: Ghostty's tagged union stores `rgb` when the tag is RGB.
            Some(CellColor::Rgb(unsafe { color.value.rgb }.into()))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColors {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub palette: [RgbColor; 256],
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

type WritePtyCallback = dyn FnMut(&[u8]) + Send;

struct WritePtyCallbackState {
    callback: Box<WritePtyCallback>,
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
    if data.is_null() && len != 0 {
        return;
    }
    let state = unsafe { &mut *(userdata.cast::<WritePtyCallbackState>()) };
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(data, len) }
    };
    (state.callback)(bytes);
}

fn install_png_decoder_once() {
    INSTALL_PNG_DECODER.call_once(|| unsafe {
        let _ = ffi::ghostty_sys_set(
            ffi::GhosttySysOption_GHOSTTY_SYS_OPT_DECODE_PNG,
            (decode_png_trampoline as *const ()).cast(),
        );
    });
}

unsafe extern "C" fn decode_png_trampoline(
    _userdata: *mut c_void,
    allocator: *const ffi::GhosttyAllocator,
    data: *const u8,
    data_len: usize,
    out: *mut ffi::GhosttySysImage,
) -> bool {
    if data.is_null() || out.is_null() {
        return false;
    }
    let bytes = unsafe { slice::from_raw_parts(data, data_len) };
    let Some(rgba) = decode_png_rgba(bytes) else {
        return false;
    };
    let ptr = unsafe { ffi::ghostty_alloc(allocator, rgba.data.len()) };
    if ptr.is_null() {
        return false;
    }
    unsafe {
        ptr::copy_nonoverlapping(rgba.data.as_ptr(), ptr, rgba.data.len());
        *out = ffi::GhosttySysImage {
            width: rgba.width,
            height: rgba.height,
            data: ptr,
            data_len: rgba.data.len(),
        };
    }
    true
}

struct DecodedPng {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn decode_png_rgba(bytes: &[u8]) -> Option<DecodedPng> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let frame = &buf[..info.buffer_size()];
    if info.bit_depth != png::BitDepth::Eight {
        return None;
    }

    let data = match info.color_type {
        png::ColorType::Rgba => frame.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((info.width as usize) * (info.height as usize) * 4);
            for rgb in frame.chunks_exact(3) {
                out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            out
        }
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((info.width as usize) * (info.height as usize) * 4);
            for gray in frame {
                out.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            out
        }
        png::ColorType::GrayscaleAlpha => {
            let mut out = Vec::with_capacity((info.width as usize) * (info.height as usize) * 4);
            for ga in frame.chunks_exact(2) {
                out.extend_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
            }
            out
        }
        png::ColorType::Indexed => return None,
    };

    Some(DecodedPng {
        width: info.width,
        height: info.height,
        data,
    })
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

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<(), Error> {
        let cell_width_px = cell_width_px.max(1);
        let cell_height_px = cell_height_px.max(1);
        // SAFETY: self.raw is valid and sizes are plain values.
        unsafe {
            ffi::ghostty_terminal_resize(self.raw, cols, rows, cell_width_px, cell_height_px)
                .into_result()
        }
    }

    pub fn enable_kitty_graphics(&mut self) -> Result<(), Error> {
        install_png_decoder_once();
        let storage_limit = KITTY_IMAGE_STORAGE_LIMIT_BYTES;
        let disable_medium = false;
        unsafe {
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT,
                (&storage_limit as *const u64).cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE,
                (&disable_medium as *const bool).cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE,
                (&disable_medium as *const bool).cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM,
                (&disable_medium as *const bool).cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES,
                (&APC_MAX_BYTES as *const usize).cast(),
            )
            .into_result()?;
            ffi::ghostty_terminal_set(
                self.raw,
                ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_APC_MAX_BYTES_KITTY,
                (&APC_MAX_BYTES_KITTY as *const usize).cast(),
            )
            .into_result()?;
        }
        Ok(())
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
        let grid_ref = self.grid_ref(ghostty_screen_point(x, y))?;
        grid_ref_graphemes(&grid_ref)
    }

    fn viewport_graphemes_and_style(&self, x: u16, y: u32) -> Result<(Vec<u32>, CellStyle), Error> {
        let grid_ref = self.grid_ref(ghostty_viewport_point(x, y))?;
        let graphemes = grid_ref_graphemes(&grid_ref)?;
        let mut style = ffi::GhosttyStyle {
            size: mem::size_of::<ffi::GhosttyStyle>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_grid_ref_style(&grid_ref, &mut style).into_result()?;
        }
        Ok((graphemes, style.into()))
    }

    pub fn viewport_hyperlink_uri(&self, x: u16, y: u32) -> Result<Option<String>, Error> {
        let grid_ref = self.grid_ref(ghostty_viewport_point(x, y))?;
        grid_ref_hyperlink_uri(&grid_ref)
    }

    fn grid_ref(&self, point: ffi::GhosttyPoint) -> Result<ffi::GhosttyGridRef, Error> {
        let mut grid_ref = ffi::GhosttyGridRef {
            size: mem::size_of::<ffi::GhosttyGridRef>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_terminal_grid_ref(self.raw, point, &mut grid_ref).into_result()?;
        }
        Ok(grid_ref)
    }

    pub fn read_text_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        self.read_formatted_selection(
            ghostty_viewport_point(start.0, start.1),
            ghostty_viewport_point(end.0, end.1),
            rectangle,
            FormatterFormat::Plain,
            true,
            true,
        )
    }

    pub fn read_ansi_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        self.read_formatted_selection(
            ghostty_viewport_point(start.0, start.1),
            ghostty_viewport_point(end.0, end.1),
            rectangle,
            FormatterFormat::Vt,
            false,
            true,
        )
    }

    pub fn read_text_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        self.read_formatted_selection(
            ghostty_screen_point(start.0, start.1),
            ghostty_screen_point(end.0, end.1),
            rectangle,
            FormatterFormat::Plain,
            true,
            true,
        )
    }

    pub fn read_ansi_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
        unwrap: bool,
    ) -> Result<String, Error> {
        self.read_formatted_selection(
            ghostty_screen_point(start.0, start.1),
            ghostty_screen_point(end.0, end.1),
            rectangle,
            FormatterFormat::Vt,
            unwrap,
            true,
        )
    }

    pub fn keyboard_state_ansi(&self) -> Result<String, Error> {
        self.format_keyboard_state_ansi(false)
    }

    pub fn kitty_keyboard_state_ansi(&self) -> Result<String, Error> {
        self.format_keyboard_state_ansi(true)
    }

    fn format_keyboard_state_ansi(&self, kitty_keyboard: bool) -> Result<String, Error> {
        let mut formatter: ffi::GhosttyFormatter_ptr = ptr::null_mut();
        let options = ffi::GhosttyFormatterTerminalOptions {
            size: mem::size_of::<ffi::GhosttyFormatterTerminalOptions>(),
            emit: FormatterFormat::Vt.as_raw(),
            unwrap: false,
            trim: false,
            extra: ffi::GhosttyFormatterTerminalExtra {
                size: mem::size_of::<ffi::GhosttyFormatterTerminalExtra>(),
                keyboard: true,
                screen: ffi::GhosttyFormatterScreenExtra {
                    size: mem::size_of::<ffi::GhosttyFormatterScreenExtra>(),
                    kitty_keyboard,
                    ..Default::default()
                },
                ..Default::default()
            },
            selection: ptr::null(),
        };
        unsafe {
            ffi::ghostty_formatter_terminal_new(ptr::null(), &mut formatter, self.raw, options)
                .into_result()?;
        }

        let mut out_ptr = ptr::null_mut();
        let mut out_len = 0usize;
        let result = unsafe {
            ffi::ghostty_formatter_format_alloc(formatter, ptr::null(), &mut out_ptr, &mut out_len)
        };
        unsafe {
            ffi::ghostty_formatter_free(formatter);
        }
        result.into_result()?;

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

    fn read_formatted_selection(
        &self,
        start: ffi::GhosttyPoint,
        end: ffi::GhosttyPoint,
        rectangle: bool,
        format: FormatterFormat,
        unwrap: bool,
        trim: bool,
    ) -> Result<String, Error> {
        let mut start_ref = ffi::GhosttyGridRef {
            size: mem::size_of::<ffi::GhosttyGridRef>(),
            ..Default::default()
        };
        let mut end_ref = ffi::GhosttyGridRef {
            size: mem::size_of::<ffi::GhosttyGridRef>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_terminal_grid_ref(self.raw, start, &mut start_ref).into_result()?;
            ffi::ghostty_terminal_grid_ref(self.raw, end, &mut end_ref).into_result()?;
        }

        let selection = ffi::GhosttySelection {
            size: mem::size_of::<ffi::GhosttySelection>(),
            start: start_ref,
            end: end_ref,
            rectangle,
        };
        let mut formatter: ffi::GhosttyFormatter_ptr = ptr::null_mut();
        let options = ffi::GhosttyFormatterTerminalOptions {
            size: mem::size_of::<ffi::GhosttyFormatterTerminalOptions>(),
            emit: format.as_raw(),
            unwrap,
            trim,
            extra: ffi::GhosttyFormatterTerminalExtra {
                size: mem::size_of::<ffi::GhosttyFormatterTerminalExtra>(),
                screen: ffi::GhosttyFormatterScreenExtra {
                    size: mem::size_of::<ffi::GhosttyFormatterScreenExtra>(),
                    ..Default::default()
                },
                ..Default::default()
            },
            selection: &selection,
        };
        unsafe {
            ffi::ghostty_formatter_terminal_new(ptr::null(), &mut formatter, self.raw, options)
                .into_result()?;
        }

        let mut out_ptr = ptr::null_mut();
        let mut out_len = 0usize;
        let result = unsafe {
            ffi::ghostty_formatter_format_alloc(formatter, ptr::null(), &mut out_ptr, &mut out_len)
        };
        unsafe {
            ffi::ghostty_formatter_free(formatter);
        }
        result.into_result()?;

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

    fn width_px(&self) -> Result<u32, Error> {
        self.get_u32(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_WIDTH_PX)
    }

    fn height_px(&self) -> Result<u32, Error> {
        self.get_u32(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_HEIGHT_PX)
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

    fn get_u32(&self, data: ffi::GhosttyTerminalData) -> Result<u32, Error> {
        let mut out = 0u32;
        // SAFETY: out points to a u32 matching the requested terminal data type.
        unsafe {
            ffi::ghostty_terminal_get(self.raw, data, (&mut out as *mut u32).cast())
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

    pub fn kitty_image_placements(&self) -> Result<Vec<KittyImagePlacement>, Error> {
        self.kitty_image_placements_with_data_filter(|_| true)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        mut needs_data: F,
    ) -> Result<Vec<KittyImagePlacement>, Error>
    where
        F: FnMut(KittyImageDescriptor) -> bool,
    {
        let mut graphics: ffi::GhosttyKittyGraphics = ptr::null_mut();
        unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                (&mut graphics as *mut ffi::GhosttyKittyGraphics).cast(),
            )
            .into_result()?;
        }
        if graphics.is_null() {
            return Ok(Vec::new());
        }

        let mut iterator: ffi::GhosttyKittyGraphicsPlacementIterator = ptr::null_mut();
        unsafe {
            ffi::ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut iterator)
                .into_result()?;
            ffi::ghostty_kitty_graphics_get(
                graphics,
                ffi::GhosttyKittyGraphicsData_GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
                (&mut iterator as *mut ffi::GhosttyKittyGraphicsPlacementIterator).cast(),
            )
            .into_result()?;
        }
        let _guard = KittyPlacementIteratorGuard { raw: iterator };

        let mut placements = Vec::new();
        while unsafe { ffi::ghostty_kitty_graphics_placement_next(iterator) } {
            if let Some(placement) =
                self.kitty_image_placement(graphics, iterator, &mut needs_data)?
            {
                placements.push(placement);
            }
        }
        placements.extend(self.kitty_virtual_image_placements(graphics, &mut needs_data)?);
        placements.sort_by_key(|placement| placement.z);
        Ok(placements)
    }

    fn kitty_image_placement<F>(
        &self,
        graphics: ffi::GhosttyKittyGraphics,
        iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
        needs_data: &mut F,
    ) -> Result<Option<KittyImagePlacement>, Error>
    where
        F: FnMut(KittyImageDescriptor) -> bool,
    {
        let image_id = kitty_placement_u32(
            iterator,
            ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
        )?;
        if kitty_placement_bool(iterator, KITTY_PLACEMENT_DATA_IS_VIRTUAL)? {
            return Ok(None);
        }
        let image = unsafe { ffi::ghostty_kitty_graphics_image(graphics, image_id) };
        if image.is_null() {
            return Ok(None);
        }

        let mut raw_info = ffi::GhosttyKittyGraphicsPlacementRenderInfo {
            size: mem::size_of::<ffi::GhosttyKittyGraphicsPlacementRenderInfo>(),
            ..Default::default()
        };
        unsafe {
            ffi::ghostty_kitty_graphics_placement_render_info(
                iterator,
                image,
                self.raw,
                &mut raw_info,
            )
            .into_result()?;
        }
        if !raw_info.viewport_visible {
            return Ok(None);
        }

        let image_width = kitty_image_u32(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
        )?;
        let image_height = kitty_image_u32(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
        )?;
        let format = kitty_image_format(image)?;
        let compression = kitty_image_compression(image)?;
        if compression != ffi::GhosttyKittyImageCompression_GHOSTTY_KITTY_IMAGE_COMPRESSION_NONE {
            return Ok(None);
        }
        let placement_id = kitty_placement_u32(
            iterator,
            ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID,
        )?;
        let (data_ptr, data_len) = kitty_image_data_ptr_len(image)?;
        let data_fingerprint =
            kitty_image_fingerprint(data_ptr, data_len, image_width, image_height, format);
        let descriptor = KittyImageDescriptor {
            image_id,
            placement_id,
            image_width,
            image_height,
            format,
            data_len,
            data_fingerprint,
        };
        let data = if needs_data(descriptor) {
            kitty_image_data_from_ptr(data_ptr, data_len)
        } else {
            Vec::new()
        };
        let x_offset = kitty_placement_u32(
            iterator,
            ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_X_OFFSET,
        )?;
        let y_offset = kitty_placement_u32(
            iterator,
            ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Y_OFFSET,
        )?;
        let z = kitty_placement_i32(
            iterator,
            ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z,
        )?;

        Ok(Some(KittyImagePlacement {
            image_id,
            placement_id,
            z,
            x_offset,
            y_offset,
            image_width,
            image_height,
            format,
            data_len,
            data_fingerprint,
            data,
            render: KittyPlacementRenderInfo {
                pixel_width: raw_info.pixel_width,
                pixel_height: raw_info.pixel_height,
                grid_cols: raw_info.grid_cols,
                grid_rows: raw_info.grid_rows,
                viewport_col: raw_info.viewport_col,
                viewport_row: raw_info.viewport_row,
                source_x: raw_info.source_x,
                source_y: raw_info.source_y,
                source_width: raw_info.source_width,
                source_height: raw_info.source_height,
            },
        }))
    }

    fn kitty_virtual_image_placements<F>(
        &self,
        graphics: ffi::GhosttyKittyGraphics,
        needs_data: &mut F,
    ) -> Result<Vec<KittyImagePlacement>, Error>
    where
        F: FnMut(KittyImageDescriptor) -> bool,
    {
        let specs = kitty_virtual_placement_specs(graphics)?;
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        let viewport_cols = self.cols()?.max(1);
        let viewport_rows = self.rows()?.max(1);
        let cell_width = (self.width_px()? / u32::from(viewport_cols)).max(1);
        let cell_height = (self.height_px()? / u32::from(viewport_rows)).max(1);
        let mut runs = Vec::new();
        for y in 0..viewport_rows {
            let mut current: Option<KittyVirtualRun> = None;
            for x in 0..viewport_cols {
                let (graphemes, style) = self.viewport_graphemes_and_style(x, u32::from(y))?;
                let cell = kitty_virtual_cell(x, y, &graphemes, style);
                match cell {
                    Some(cell) => {
                        if let Some(run) = current.as_mut() {
                            if run.append(cell) {
                                continue;
                            }
                            runs.push(*run);
                        }
                        current = Some(KittyVirtualRun::from_cell(cell));
                    }
                    None => {
                        if let Some(run) = current.take() {
                            runs.push(run);
                        }
                    }
                }
            }
            if let Some(run) = current {
                runs.push(run);
            }
        }

        let mut placements = Vec::new();
        for run in runs {
            let image_id = run.image_id();
            let Some(spec) = find_virtual_placement_spec(&specs, image_id, run.placement_id())
            else {
                continue;
            };
            let image = unsafe { ffi::ghostty_kitty_graphics_image(graphics, image_id) };
            if image.is_null() {
                continue;
            }
            let image_width = kitty_image_u32(
                image,
                ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_WIDTH,
            )?;
            let image_height = kitty_image_u32(
                image,
                ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_HEIGHT,
            )?;
            let format = kitty_image_format(image)?;
            let compression = kitty_image_compression(image)?;
            if compression != ffi::GhosttyKittyImageCompression_GHOSTTY_KITTY_IMAGE_COMPRESSION_NONE
            {
                continue;
            }
            let Some(geometry) = kitty_virtual_placement_geometry(
                run,
                *spec,
                image_width,
                image_height,
                cell_width,
                cell_height,
            ) else {
                continue;
            };
            let placement_id = run.synthetic_placement_id();
            let (data_ptr, data_len) = kitty_image_data_ptr_len(image)?;
            let data_fingerprint =
                kitty_image_fingerprint(data_ptr, data_len, image_width, image_height, format);
            let descriptor = KittyImageDescriptor {
                image_id,
                placement_id,
                image_width,
                image_height,
                format,
                data_len,
                data_fingerprint,
            };
            let data = if needs_data(descriptor) {
                kitty_image_data_from_ptr(data_ptr, data_len)
            } else {
                Vec::new()
            };
            placements.push(KittyImagePlacement {
                image_id,
                placement_id,
                z: spec.z,
                x_offset: geometry.x_offset,
                y_offset: geometry.y_offset,
                image_width,
                image_height,
                format,
                data_len,
                data_fingerprint,
                data,
                render: geometry.render,
            });
        }

        Ok(placements)
    }

    fn raw(&self) -> ffi::GhosttyTerminal_ptr {
        self.raw
    }
}

struct KittyPlacementIteratorGuard {
    raw: ffi::GhosttyKittyGraphicsPlacementIterator,
}

impl Drop for KittyPlacementIteratorGuard {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_kitty_graphics_placement_iterator_free(self.raw) }
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

fn ghostty_screen_point(x: u16, y: u32) -> ffi::GhosttyPoint {
    ffi::GhosttyPoint {
        tag: ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_SCREEN,
        value: ffi::GhosttyPointValue {
            coordinate: ffi::GhosttyPointCoordinate { x, y },
        },
    }
}

fn grid_ref_graphemes(grid_ref: &ffi::GhosttyGridRef) -> Result<Vec<u32>, Error> {
    let mut required = 0usize;
    let result =
        unsafe { ffi::ghostty_grid_ref_graphemes(grid_ref, ptr::null_mut(), 0, &mut required) };
    if result != ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
        result.into_result()?;
    }
    let mut buffer = vec![0u32; required];
    if required == 0 {
        return Ok(buffer);
    }
    unsafe {
        ffi::ghostty_grid_ref_graphemes(grid_ref, buffer.as_mut_ptr(), buffer.len(), &mut required)
            .into_result()?;
    }
    buffer.truncate(required);
    Ok(buffer)
}

fn kitty_placement_u32(
    iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
    data: ffi::GhosttyKittyGraphicsPlacementData,
) -> Result<u32, Error> {
    let mut out = 0u32;
    unsafe {
        ffi::ghostty_kitty_graphics_placement_get(iterator, data, (&mut out as *mut u32).cast())
            .into_result()?;
    }
    Ok(out)
}

fn kitty_placement_i32(
    iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
    data: ffi::GhosttyKittyGraphicsPlacementData,
) -> Result<i32, Error> {
    let mut out = 0i32;
    unsafe {
        ffi::ghostty_kitty_graphics_placement_get(iterator, data, (&mut out as *mut i32).cast())
            .into_result()?;
    }
    Ok(out)
}

fn kitty_placement_bool(
    iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
    data: ffi::GhosttyKittyGraphicsPlacementData,
) -> Result<bool, Error> {
    let mut out = false;
    unsafe {
        ffi::ghostty_kitty_graphics_placement_get(iterator, data, (&mut out as *mut bool).cast())
            .into_result()?;
    }
    Ok(out)
}

fn kitty_virtual_placement_specs(
    graphics: ffi::GhosttyKittyGraphics,
) -> Result<Vec<KittyVirtualPlacementSpec>, Error> {
    let mut iterator: ffi::GhosttyKittyGraphicsPlacementIterator = ptr::null_mut();
    unsafe {
        ffi::ghostty_kitty_graphics_placement_iterator_new(ptr::null(), &mut iterator)
            .into_result()?;
        ffi::ghostty_kitty_graphics_get(
            graphics,
            ffi::GhosttyKittyGraphicsData_GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
            (&mut iterator as *mut ffi::GhosttyKittyGraphicsPlacementIterator).cast(),
        )
        .into_result()?;
    }
    let _guard = KittyPlacementIteratorGuard { raw: iterator };

    let mut specs = Vec::new();
    while unsafe { ffi::ghostty_kitty_graphics_placement_next(iterator) } {
        if !kitty_placement_bool(iterator, KITTY_PLACEMENT_DATA_IS_VIRTUAL)? {
            continue;
        }
        specs.push(KittyVirtualPlacementSpec {
            image_id: kitty_placement_u32(
                iterator,
                ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID,
            )?,
            placement_id: kitty_placement_u32(
                iterator,
                ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID,
            )?,
            columns: kitty_placement_u32(iterator, KITTY_PLACEMENT_DATA_COLUMNS)?,
            rows: kitty_placement_u32(iterator, KITTY_PLACEMENT_DATA_ROWS)?,
            z: kitty_placement_i32(
                iterator,
                ffi::GhosttyKittyGraphicsPlacementData_GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z,
            )?,
        });
    }
    Ok(specs)
}

fn find_virtual_placement_spec(
    specs: &[KittyVirtualPlacementSpec],
    image_id: u32,
    placement_id: u32,
) -> Option<&KittyVirtualPlacementSpec> {
    if placement_id > 0 {
        specs
            .iter()
            .find(|spec| spec.image_id == image_id && spec.placement_id == placement_id)
    } else {
        specs.iter().find(|spec| spec.image_id == image_id)
    }
}

fn kitty_virtual_cell(
    x: u16,
    y: u16,
    graphemes: &[u32],
    style: CellStyle,
) -> Option<KittyVirtualCell> {
    if graphemes.first().copied() != Some(KITTY_UNICODE_PLACEHOLDER) {
        return None;
    }
    let image_id_low = style
        .fg_color
        .map(kitty_placeholder_color_to_id)
        .unwrap_or(0);
    let placement_id = style
        .underline_color
        .map(kitty_placeholder_color_to_id)
        .filter(|id| *id != 0);
    let row = graphemes
        .get(1)
        .and_then(|codepoint| kitty_placeholder_diacritic_index(*codepoint));
    let col = graphemes
        .get(2)
        .and_then(|codepoint| kitty_placeholder_diacritic_index(*codepoint));
    let image_id_high = graphemes
        .get(3)
        .and_then(|codepoint| kitty_placeholder_diacritic_index(*codepoint))
        .filter(|high| *high <= u32::from(u8::MAX));

    Some(KittyVirtualCell {
        x,
        y,
        image_id_low,
        image_id_high,
        placement_id,
        row,
        col,
    })
}

fn kitty_placeholder_color_to_id(color: CellColor) -> u32 {
    match color {
        CellColor::Palette(value) => value.into(),
        CellColor::Rgb(color) => {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        }
    }
}

fn kitty_placeholder_diacritic_index(codepoint: u32) -> Option<u32> {
    let map = KITTY_PLACEHOLDER_DIACRITICS.get_or_init(|| {
        // Reuse Ghostty's vendored table so Herdr decodes the same placeholder
        // row/column diacritics that libghostty accepts.
        let source =
            include_str!("../../vendor/libghostty-vt/src/terminal/kitty/graphics_unicode.zig");
        let mut map = HashMap::new();
        let mut in_table = false;
        for line in source.lines() {
            let line = line.trim();
            if line.starts_with("const diacritics:") {
                in_table = true;
                continue;
            }
            if !in_table {
                continue;
            }
            if line == "};" {
                break;
            }
            let Some(hex) = line
                .strip_prefix("0x")
                .and_then(|value| value.strip_suffix(','))
            else {
                continue;
            };
            if let Ok(value) = u32::from_str_radix(hex, 16) {
                map.insert(value, map.len() as u32);
            }
        }
        map
    });
    map.get(&codepoint).copied()
}

fn kitty_virtual_placement_geometry(
    run: KittyVirtualRun,
    spec: KittyVirtualPlacementSpec,
    image_width: u32,
    image_height: u32,
    cell_width: u32,
    cell_height: u32,
) -> Option<KittyVirtualPlacementGeometry> {
    let grid_cols = if spec.columns == 0 {
        image_width.saturating_add(cell_width - 1) / cell_width
    } else {
        spec.columns
    }
    .max(1);
    let grid_rows = if spec.rows == 0 {
        image_height.saturating_add(cell_height - 1) / cell_height
    } else {
        spec.rows
    }
    .max(1);

    if run.col >= grid_cols || run.row >= grid_rows {
        return None;
    }
    let visible_cols = run.width.min(grid_cols.saturating_sub(run.col)).max(1);
    let visible_rows = 1;
    let source_x = scale_u32(run.col, image_width, grid_cols);
    let source_y = scale_u32(run.row, image_height, grid_rows);
    let source_width = scale_u32(visible_cols, image_width, grid_cols)
        .max(1)
        .min(image_width.saturating_sub(source_x));
    let source_height = scale_u32(visible_rows, image_height, grid_rows)
        .max(1)
        .min(image_height.saturating_sub(source_y));
    if source_width == 0 || source_height == 0 {
        return None;
    }

    Some(KittyVirtualPlacementGeometry {
        x_offset: 0,
        y_offset: 0,
        render: KittyPlacementRenderInfo {
            pixel_width: visible_cols.saturating_mul(cell_width).max(1),
            pixel_height: visible_rows.saturating_mul(cell_height).max(1),
            grid_cols: visible_cols,
            grid_rows: visible_rows,
            viewport_col: i32::from(run.x),
            viewport_row: i32::from(run.y),
            source_x,
            source_y,
            source_width,
            source_height,
        },
    })
}

fn scale_u32(value: u32, source: u32, dest: u32) -> u32 {
    ((u64::from(value)).saturating_mul(u64::from(source)) / u64::from(dest.max(1)))
        .min(u64::from(u32::MAX)) as u32
}

impl KittyVirtualRun {
    fn from_cell(cell: KittyVirtualCell) -> Self {
        Self {
            x: cell.x,
            y: cell.y,
            image_id_low: cell.image_id_low,
            image_id_high: cell.image_id_high,
            placement_id: cell.placement_id,
            row: cell.row.unwrap_or(0),
            col: cell.col.unwrap_or(0),
            width: 1,
        }
    }

    fn append(&mut self, cell: KittyVirtualCell) -> bool {
        if self.image_id_low != cell.image_id_low
            || self.placement_id != cell.placement_id
            || cell.row.is_some_and(|row| row != self.row)
            || cell.col.is_some_and(|col| col != self.col + self.width)
            || cell
                .image_id_high
                .is_some_and(|high| Some(high) != self.image_id_high)
        {
            return false;
        }
        self.width += 1;
        true
    }

    fn image_id(self) -> u32 {
        self.image_id_low | (self.image_id_high.unwrap_or(0) << 24)
    }

    fn placement_id(self) -> u32 {
        self.placement_id.unwrap_or(0)
    }

    fn synthetic_placement_id(self) -> u32 {
        let mut hasher = DefaultHasher::new();
        self.image_id().hash(&mut hasher);
        self.placement_id().hash(&mut hasher);
        self.row.hash(&mut hasher);
        self.col.hash(&mut hasher);
        self.width.hash(&mut hasher);
        self.x.hash(&mut hasher);
        self.y.hash(&mut hasher);
        1 + ((hasher.finish() as u32) % 900_000)
    }
}

fn kitty_image_u32(
    image: ffi::GhosttyKittyGraphicsImage,
    data: ffi::GhosttyKittyGraphicsImageData,
) -> Result<u32, Error> {
    let mut out = 0u32;
    unsafe {
        ffi::ghostty_kitty_graphics_image_get(image, data, (&mut out as *mut u32).cast())
            .into_result()?;
    }
    Ok(out)
}

fn kitty_image_format(image: ffi::GhosttyKittyGraphicsImage) -> Result<KittyImageFormat, Error> {
    let mut out = ffi::GhosttyKittyImageFormat_GHOSTTY_KITTY_IMAGE_FORMAT_RGBA;
    unsafe {
        ffi::ghostty_kitty_graphics_image_get(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_FORMAT,
            (&mut out as *mut ffi::GhosttyKittyImageFormat).cast(),
        )
        .into_result()?;
    }
    match out {
        ffi::GhosttyKittyImageFormat_GHOSTTY_KITTY_IMAGE_FORMAT_RGB => Ok(KittyImageFormat::Rgb),
        ffi::GhosttyKittyImageFormat_GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => Ok(KittyImageFormat::Rgba),
        ffi::GhosttyKittyImageFormat_GHOSTTY_KITTY_IMAGE_FORMAT_PNG => Ok(KittyImageFormat::Png),
        _ => Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE)),
    }
}

fn kitty_image_compression(
    image: ffi::GhosttyKittyGraphicsImage,
) -> Result<ffi::GhosttyKittyImageCompression, Error> {
    let mut out = ffi::GhosttyKittyImageCompression_GHOSTTY_KITTY_IMAGE_COMPRESSION_NONE;
    unsafe {
        ffi::ghostty_kitty_graphics_image_get(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_COMPRESSION,
            (&mut out as *mut ffi::GhosttyKittyImageCompression).cast(),
        )
        .into_result()?;
    }
    Ok(out)
}

fn kitty_image_data_ptr_len(
    image: ffi::GhosttyKittyGraphicsImage,
) -> Result<(*const u8, usize), Error> {
    let mut ptr_out: *const u8 = ptr::null();
    let mut len = 0usize;
    unsafe {
        ffi::ghostty_kitty_graphics_image_get(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
            (&mut ptr_out as *mut *const u8).cast(),
        )
        .into_result()?;
        ffi::ghostty_kitty_graphics_image_get(
            image,
            ffi::GhosttyKittyGraphicsImageData_GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN,
            (&mut len as *mut usize).cast(),
        )
        .into_result()?;
    }
    Ok((ptr_out, len))
}

fn kitty_image_data_from_ptr(ptr_out: *const u8, len: usize) -> Vec<u8> {
    if ptr_out.is_null() || len == 0 {
        return Vec::new();
    }
    unsafe { slice::from_raw_parts(ptr_out, len) }.to_vec()
}

fn kitty_image_fingerprint(
    ptr_out: *const u8,
    len: usize,
    image_width: u32,
    image_height: u32,
    format: KittyImageFormat,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    len.hash(&mut hasher);
    image_width.hash(&mut hasher);
    image_height.hash(&mut hasher);
    format.hash(&mut hasher);
    if ptr_out.is_null() || len == 0 {
        return hasher.finish();
    }

    let data = unsafe { slice::from_raw_parts(ptr_out, len) };
    hash_kitty_image_data_sample(&mut hasher, data);
    hasher.finish()
}

fn hash_kitty_image_data_sample(hasher: &mut impl Hasher, data: &[u8]) {
    let sample = KITTY_FINGERPRINT_SAMPLE_BYTES;
    if data.len() <= sample * 3 {
        data.hash(hasher);
        return;
    }

    data[..sample].hash(hasher);
    let middle_start = (data.len() / 2).saturating_sub(sample / 2);
    data[middle_start..middle_start + sample].hash(hasher);
    data[data.len() - sample..].hash(hasher);
}

fn grid_ref_hyperlink_uri(grid_ref: &ffi::GhosttyGridRef) -> Result<Option<String>, Error> {
    let mut required = 0usize;
    let result =
        unsafe { ffi::ghostty_grid_ref_hyperlink_uri(grid_ref, ptr::null_mut(), 0, &mut required) };
    if result != ffi::GhosttyResult_GHOSTTY_OUT_OF_SPACE {
        result.into_result()?;
    }
    if required == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0u8; required];
    unsafe {
        ffi::ghostty_grid_ref_hyperlink_uri(
            grid_ref,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut required,
        )
        .into_result()?;
    }
    buffer.truncate(required);
    Ok(Some(String::from_utf8_lossy(&buffer).into_owned()))
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

    pub fn cursor_blinking(&self) -> Result<bool, Error> {
        self.get_bool(ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_BLINKING)
    }

    pub fn cursor_visual_style(&self) -> Result<CursorVisualStyle, Error> {
        let mut out: ffi::GhosttyRenderStateCursorVisualStyle = 0;
        // SAFETY: out points to the matching enum storage for the requested data kind.
        unsafe {
            ffi::ghostty_render_state_get(
                self.raw,
                ffi::GhosttyRenderStateData_GHOSTTY_RENDER_STATE_DATA_CURSOR_VISUAL_STYLE,
                (&mut out as *mut ffi::GhosttyRenderStateCursorVisualStyle).cast(),
            )
            .into_result()?;
        }
        Ok(CursorVisualStyle::from_raw(out))
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
            palette: colors.palette.map(Into::into),
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

// SAFETY: the opaque encoder handle is only used behind external synchronization in pane runtime.
unsafe impl Send for KeyEncoder {}

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

    pub fn set_format(&mut self, format: ffi::GhosttyMouseFormat) {
        unsafe {
            ffi::ghostty_mouse_encoder_setopt(
                self.raw,
                ffi::GhosttyMouseEncoderOption_GHOSTTY_MOUSE_ENCODER_OPT_FORMAT,
                (&format as *const ffi::GhosttyMouseFormat).cast(),
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

    pub fn clear_dirty(&mut self) -> Result<(), Error> {
        self.set_dirty(false)
    }

    pub fn set_dirty(&mut self, dirty: bool) -> Result<(), Error> {
        // SAFETY: dirty pointer matches the expected row option type.
        unsafe {
            ffi::ghostty_render_state_row_set(
                self.iterator.raw,
                ffi::GhosttyRenderStateRowOption_GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY,
                (&dirty as *const bool).cast(),
            )
            .into_result()
        }
    }

    pub fn selection(&self) -> Result<Option<RowSelection>, Error> {
        let mut selection = ffi::GhosttyRenderStateRowSelection {
            size: mem::size_of::<ffi::GhosttyRenderStateRowSelection>(),
            ..Default::default()
        };
        let result = unsafe {
            ffi::ghostty_render_state_row_get(
                self.iterator.raw,
                ffi::GhosttyRenderStateRowData_GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION,
                (&mut selection as *mut ffi::GhosttyRenderStateRowSelection).cast(),
            )
        };
        match result {
            ffi::GhosttyResult_GHOSTTY_SUCCESS => Ok(Some(RowSelection {
                start_x: selection.start_x,
                end_x: selection.end_x,
            })),
            ffi::GhosttyResult_GHOSTTY_NO_VALUE => Ok(None),
            other => Err(Error(other)),
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellBasicData {
    pub wide: CellWide,
    pub has_hyperlink: bool,
    pub has_styling: bool,
    pub style: CellStyle,
}

impl Default for CellBasicData {
    fn default() -> Self {
        Self {
            wide: CellWide::Narrow,
            has_hyperlink: false,
            has_styling: false,
            style: CellStyle::default(),
        }
    }
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

    pub fn basic_data(&self) -> Result<CellBasicData, Error> {
        let mut raw = ffi::GhosttyCell::default();
        let mut style = ffi::GhosttyStyle {
            size: mem::size_of::<ffi::GhosttyStyle>(),
            ..Default::default()
        };
        let mut has_styling = false;
        let row_keys = [
            ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,
            ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,
            ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_HAS_STYLING,
        ];
        let mut row_values = [
            (&mut raw as *mut ffi::GhosttyCell).cast::<c_void>(),
            (&mut style as *mut ffi::GhosttyStyle).cast::<c_void>(),
            (&mut has_styling as *mut bool).cast::<c_void>(),
        ];
        let mut written = 0usize;
        unsafe {
            ffi::ghostty_render_state_row_cells_get_multi(
                self.cells.raw,
                row_keys.len(),
                row_keys.as_ptr(),
                row_values.as_mut_ptr(),
                &mut written,
            )
            .into_result()?;
        }

        let mut wide = ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_NARROW;
        let mut has_hyperlink = false;
        let cell_keys = [
            ffi::GhosttyCellData_GHOSTTY_CELL_DATA_WIDE,
            ffi::GhosttyCellData_GHOSTTY_CELL_DATA_HAS_HYPERLINK,
        ];
        let mut cell_values = [
            (&mut wide as *mut ffi::GhosttyCellWide).cast::<c_void>(),
            (&mut has_hyperlink as *mut bool).cast::<c_void>(),
        ];
        unsafe {
            ffi::ghostty_cell_get_multi(
                raw,
                cell_keys.len(),
                cell_keys.as_ptr(),
                cell_values.as_mut_ptr(),
                &mut written,
            )
            .into_result()?;
        }

        Ok(CellBasicData {
            wide: CellWide::from_raw(wide),
            has_hyperlink,
            has_styling,
            style: style.into(),
        })
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

    pub fn has_hyperlink(&self) -> Result<bool, Error> {
        let raw = self.raw_cell()?;
        let mut has_hyperlink = false;
        unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_HAS_HYPERLINK,
                (&mut has_hyperlink as *mut bool).cast(),
            )
            .into_result()?;
        }
        Ok(has_hyperlink)
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

    pub fn content_bg_color(&self) -> Result<Option<CellColor>, Error> {
        let raw = self.raw_cell()?;
        let mut tag = ffi::GhosttyCellContentTag_GHOSTTY_CELL_CONTENT_CODEPOINT;
        unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_CONTENT_TAG,
                (&mut tag as *mut ffi::GhosttyCellContentTag).cast(),
            )
            .into_result()?;
        }

        match tag {
            ffi::GhosttyCellContentTag_GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE => {
                let mut index = 0u8;
                unsafe {
                    ffi::ghostty_cell_get(
                        raw,
                        ffi::GhosttyCellData_GHOSTTY_CELL_DATA_COLOR_PALETTE,
                        (&mut index as *mut u8).cast(),
                    )
                    .into_result()?;
                }
                Ok(Some(CellColor::Palette(index)))
            }
            ffi::GhosttyCellContentTag_GHOSTTY_CELL_CONTENT_BG_COLOR_RGB => {
                let mut color = ffi::GhosttyColorRgb::default();
                unsafe {
                    ffi::ghostty_cell_get(
                        raw,
                        ffi::GhosttyCellData_GHOSTTY_CELL_DATA_COLOR_RGB,
                        (&mut color as *mut ffi::GhosttyColorRgb).cast(),
                    )
                    .into_result()?;
                }
                Ok(Some(CellColor::Rgb(color.into())))
            }
            _ => Ok(None),
        }
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

    fn raw_cell_text_into(&self, text: &mut String) -> Result<(), Error> {
        let raw = self.raw_cell()?;
        let mut has_text = false;
        unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_HAS_TEXT,
                (&mut has_text as *mut bool).cast(),
            )
            .into_result()?;
        }
        if !has_text {
            return Ok(());
        }

        let mut codepoint = 0u32;
        unsafe {
            ffi::ghostty_cell_get(
                raw,
                ffi::GhosttyCellData_GHOSTTY_CELL_DATA_CODEPOINT,
                (&mut codepoint as *mut u32).cast(),
            )
            .into_result()?;
        }
        if let Some(ch) = char::from_u32(codepoint) {
            text.push(ch);
        }
        Ok(())
    }

    pub fn grapheme_text(&self) -> Result<String, Error> {
        let mut codepoints = Vec::new();
        let mut text = String::new();
        self.grapheme_text_into(&mut codepoints, &mut text)?;
        Ok(text)
    }

    pub fn grapheme_text_into(
        &self,
        codepoints: &mut Vec<u32>,
        text: &mut String,
    ) -> Result<(), Error> {
        text.clear();
        codepoints.clear();

        let mut len = 0u32;
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,
                (&mut len as *mut u32).cast(),
            )
            .into_result()?;
        };

        if len == 0 {
            return self.raw_cell_text_into(text);
        }

        // Avoid GRAPHEMES_UTF8 here. In libghostty-vt 0.6.7's vendor, its
        // required-byte query can underreport multi-codepoint graphemes.
        codepoints.resize(len as usize, 0);
        unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.cells.raw,
                ffi::GhosttyRenderStateRowCellsData_GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,
                codepoints.as_mut_ptr().cast(),
            )
            .into_result()?;
        }

        for codepoint in codepoints.iter().copied() {
            match char::from_u32(codepoint) {
                Some(ch) => text.push(ch),
                None => text.push(char::REPLACEMENT_CHARACTER),
            }
        }
        Ok(())
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
        terminal.write(line.repeat(count).as_bytes());
    }

    fn build_info_bool(data: ffi::GhosttyBuildInfo) -> bool {
        let mut out = false;
        unsafe {
            ffi::ghostty_build_info(data, (&mut out as *mut bool).cast())
                .into_result()
                .unwrap();
        }
        out
    }

    fn build_info_optimize() -> ffi::GhosttyOptimizeMode {
        let mut out = ffi::GhosttyOptimizeMode_GHOSTTY_OPTIMIZE_DEBUG;
        unsafe {
            ffi::ghostty_build_info(
                ffi::GhosttyBuildInfo_GHOSTTY_BUILD_INFO_OPTIMIZE,
                (&mut out as *mut ffi::GhosttyOptimizeMode).cast(),
            )
            .into_result()
            .unwrap();
        }
        out
    }

    #[test]
    fn kitty_image_fingerprint_samples_large_payloads() {
        let mut data = vec![1u8; KITTY_FINGERPRINT_SAMPLE_BYTES * 4];
        let original =
            kitty_image_fingerprint(data.as_ptr(), data.len(), 100, 50, KittyImageFormat::Png);

        data[KITTY_FINGERPRINT_SAMPLE_BYTES / 2] = 2;
        let changed_prefix =
            kitty_image_fingerprint(data.as_ptr(), data.len(), 100, 50, KittyImageFormat::Png);
        assert_ne!(original, changed_prefix);

        data[KITTY_FINGERPRINT_SAMPLE_BYTES / 2] = 1;
        let middle = data.len() / 2;
        data[middle] = 3;
        let changed_middle =
            kitty_image_fingerprint(data.as_ptr(), data.len(), 100, 50, KittyImageFormat::Png);
        assert_ne!(original, changed_middle);

        data[middle] = 1;
        let last = data.len() - 1;
        data[last] = 4;
        let changed_suffix =
            kitty_image_fingerprint(data.as_ptr(), data.len(), 100, 50, KittyImageFormat::Png);
        assert_ne!(original, changed_suffix);
    }

    #[test]
    fn build_info_contract_matches_expected_vendored_features() {
        let _simd = build_info_bool(ffi::GhosttyBuildInfo_GHOSTTY_BUILD_INFO_SIMD);
        let _tmux_control_mode =
            build_info_bool(ffi::GhosttyBuildInfo_GHOSTTY_BUILD_INFO_TMUX_CONTROL_MODE);
        let _kitty_graphics =
            build_info_bool(ffi::GhosttyBuildInfo_GHOSTTY_BUILD_INFO_KITTY_GRAPHICS);

        let optimize = build_info_optimize();
        assert!(matches!(
            optimize,
            ffi::GhosttyOptimizeMode_GHOSTTY_OPTIMIZE_DEBUG
                | ffi::GhosttyOptimizeMode_GHOSTTY_OPTIMIZE_RELEASE_SAFE
                | ffi::GhosttyOptimizeMode_GHOSTTY_OPTIMIZE_RELEASE_SMALL
                | ffi::GhosttyOptimizeMode_GHOSTTY_OPTIMIZE_RELEASE_FAST
        ));
    }

    #[test]
    fn kitty_graphics_direct_rgba_placement_is_queryable() {
        let mut terminal = Terminal::new(10, 5, 0).unwrap();
        terminal.enable_kitty_graphics().unwrap();
        terminal.resize(10, 5, 8, 16).unwrap();
        terminal.write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,q=2;/wAA/w==\x1b\\");

        let placements = terminal.kitty_image_placements().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].image_id, 7);
        assert_eq!(placements[0].placement_id, 3);
        assert_eq!(placements[0].image_width, 1);
        assert_eq!(placements[0].image_height, 1);
        assert_eq!(placements[0].format, KittyImageFormat::Rgba);
        assert_eq!(placements[0].data, [255, 0, 0, 255]);
        assert_eq!(placements[0].render.grid_cols, 10);
        assert_eq!(placements[0].render.grid_rows, 5);
    }

    #[test]
    fn kitty_graphics_unicode_placeholder_placement_is_queryable() {
        let mut terminal = Terminal::new(10, 5, 0).unwrap();
        terminal.enable_kitty_graphics().unwrap();
        terminal.resize(10, 5, 8, 16).unwrap();
        terminal.write(b"\x1b_Gq=2,a=t,t=d,f=32,s=1,v=1,i=1193046,m=0;/wAA/w==\x1b\\");
        terminal.write(b"\x1b_Gq=2,a=p,U=1,i=1193046,c=2,r=1\x1b\\");
        terminal.write("\x1b[2;3H\x1b[38;2;18;52;86m\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\x1b[0m".as_bytes());

        let placements = terminal.kitty_image_placements().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].image_id, 1193046);
        assert_ne!(placements[0].placement_id, 0);
        assert_eq!(placements[0].image_width, 1);
        assert_eq!(placements[0].image_height, 1);
        assert_eq!(placements[0].format, KittyImageFormat::Rgba);
        assert_eq!(placements[0].data, [255, 0, 0, 255]);
        assert_eq!(placements[0].render.viewport_col, 2);
        assert_eq!(placements[0].render.viewport_row, 1);
        assert_eq!(placements[0].render.grid_cols, 2);
        assert_eq!(placements[0].render.grid_rows, 1);
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
    fn resize_reflows_soft_wrapped_scrollback_to_new_width() {
        // #77 layer 2: the vendored libghostty-vt `ghostty_terminal_resize`
        // already routes through the reflowing screen/pagelist path (wraparound
        // is on by default), so widening the terminal must un-wrap history that
        // was soft-wrapped at the old narrow width — not leave it hard-wrapped.
        //
        // Width 6 forces "ABCDEFGHIJKL" to soft-wrap across two rows; trailing
        // newlines then push that wrapped pair down into scrollback history.
        let mut terminal = Terminal::new(6, 2, 10_000).unwrap();
        terminal.write(b"ABCDEFGHIJKL\r\nx\r\ny\r\nz\r\n");

        let wrapped_scrollback = terminal.scrollback_rows().unwrap();

        // Widen past the logical line length: a reflowing resize collapses the
        // two soft-wrapped scrollback rows back into one, so history shrinks.
        terminal.resize(20, 2, 0, 0).unwrap();
        let reflowed_scrollback = terminal.scrollback_rows().unwrap();
        assert!(
            reflowed_scrollback < wrapped_scrollback,
            "expected widen to reflow wrapped history into fewer rows, \
             got wrapped={wrapped_scrollback}, reflowed={reflowed_scrollback}"
        );

        // The reflowed history reads back as one unwrapped logical line.
        let line = terminal.read_text_screen((0, 0), (19, 0), false).unwrap();
        assert!(
            line.starts_with("ABCDEFGHIJKL"),
            "expected reflowed history to read as one line, got {line:?}"
        );
    }

    #[test]
    fn terminal_extracts_viewport_hyperlink_uri() {
        let mut terminal = Terminal::new(20, 3, 0).unwrap();
        terminal.write(b"\x1b]8;;https://example.com\x1b\\Link\x1b]8;;\x1b\\");

        assert_eq!(
            terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
            Some("https://example.com")
        );
        assert_eq!(terminal.viewport_hyperlink_uri(4, 0).unwrap(), None);
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

        write_padded_lines(&mut small, 1_250, 70);
        write_padded_lines(&mut large, 1_250, 70);

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
    fn active_screen_and_cursor_visibility_contract() {
        let mut terminal = Terminal::new(12, 3, 0).unwrap();
        let mut render_state = RenderState::new().unwrap();

        terminal.write(b"primary");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
            "primary"
        );

        render_state.update(&terminal).unwrap();
        assert!(render_state.cursor_visible().unwrap());
        terminal.write(b"\x1b[?25l");
        render_state.update(&terminal).unwrap();
        assert!(!render_state.cursor_visible().unwrap());

        terminal.write(b"\x1b[?1049h\x1b[HALT");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Alternate);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (2, 0), false).unwrap(),
            "ALT"
        );

        terminal.write(b"\x1b[?1049l");
        assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
        assert_eq!(
            terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
            "primary"
        );
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
                let text = cells.grapheme_text().unwrap();
                if text.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&text);
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

    #[test]
    fn render_cells_handle_issue_453_unicode_payload() {
        let mut terminal = Terminal::new(80, 3, 100).unwrap();
        terminal.write("README 👨‍👩‍👧‍👦 🧑‍💻 ✅ ⚡ 漢字 café é 🏳️‍🌈 🚀\r\n".as_bytes());

        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();

        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        let mut row_cells = RowCells::new().unwrap();
        let mut codepoints = Vec::new();
        let mut text = String::new();

        while rows.next() {
            let mut cells = rows.populate_cells(&mut row_cells).unwrap();
            while cells.next() {
                cells
                    .grapheme_text_into(&mut codepoints, &mut text)
                    .unwrap();
            }
        }
    }

    #[test]
    fn render_state_row_dirty_can_be_cleared_independently() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        let mut render_state = RenderState::new().unwrap();

        render_state.update(&terminal).unwrap();
        {
            let mut row_iterator = RowIterator::new().unwrap();
            let mut rows = render_state
                .populate_row_iterator(&mut row_iterator)
                .unwrap();
            while rows.next() {
                rows.clear_dirty().unwrap();
                assert!(!rows.dirty().unwrap());
            }
        }
        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);

        terminal.write(b"A");
        render_state.update(&terminal).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

        let mut dirty_rows = 0usize;
        {
            let mut row_iterator = RowIterator::new().unwrap();
            let mut rows = render_state
                .populate_row_iterator(&mut row_iterator)
                .unwrap();
            while rows.next() {
                if rows.dirty().unwrap() {
                    dirty_rows += 1;
                    rows.clear_dirty().unwrap();
                    assert!(!rows.dirty().unwrap());
                }
            }
        }
        assert_eq!(dirty_rows, 1);
        assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

        render_state.set_dirty(Dirty::Clean).unwrap();
        assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
    }

    #[test]
    fn row_selection_returns_none_without_selection() {
        let terminal = Terminal::new(8, 3, 100).unwrap();
        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();

        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        assert!(rows.next());
        assert_eq!(rows.selection().unwrap(), None);
    }

    #[test]
    fn row_cell_basic_data_uses_batched_vendor_reads() {
        let mut terminal = Terminal::new(8, 3, 100).unwrap();
        terminal.write(b"\x1b[31mA\x1b[0m");

        let mut render_state = RenderState::new().unwrap();
        render_state.update(&terminal).unwrap();

        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        assert!(rows.next());

        let mut row_cells = RowCells::new().unwrap();
        let mut cells = rows.populate_cells(&mut row_cells).unwrap();
        assert!(cells.next());

        let basic = cells.basic_data().unwrap();
        assert_eq!(basic.wide, CellWide::Narrow);
        assert!(basic.has_styling);
        assert_eq!(basic.style.fg_color, Some(CellColor::Palette(1)));
        assert!(!basic.has_hyperlink);
    }
}
