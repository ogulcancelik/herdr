//! Cross-platform terminal capability gates used by the client renderer.
//!
//! Keeping these here lets `client/mod.rs` and `main.rs` stay free of
//! `#[cfg(target_os)]` while still skipping crossterm primitives that
//! aren't implemented on the Windows legacy console.

use std::io;

/// Switch the stdin console handle into Virtual Terminal input mode so
/// that key and mouse events arrive as VT escape sequences instead of
/// Win32 `INPUT_RECORD` structs. No-op on Unix targets.
pub fn enable_vt_input() -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        super::enable_vt_input()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}

/// Whether `crossterm::event::DisableMouseCapture` can safely be invoked
/// without a prior matching `EnableMouseCapture`. Windows' legacy console
/// backend requires the original mouse mode to have been cached by
/// `Enable*` first; calling disable cold returns "Initial console modes
/// not set". POSIX terminals tolerate the bare disable sequence.
pub const TOLERATES_UNMATCHED_MOUSE_DISABLE: bool = !cfg!(target_os = "windows");

/// Whether the kitty keyboard protocol's
/// `PushKeyboardEnhancementFlags` / `PopKeyboardEnhancementFlags` calls
/// are implemented for the current crossterm backend. They return
/// `Unsupported` on Windows.
pub const SUPPORTS_KEYBOARD_ENHANCEMENT: bool = !cfg!(target_os = "windows");
