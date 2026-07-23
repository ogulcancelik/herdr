use std::io::{self, Write};

#[cfg(any(not(windows), test))]
const DISABLE_HOST_MOUSE_REPORTING_SEQUENCE: &[u8] =
    b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(not(windows))]
pub(crate) fn clear_host_mouse_reporting<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE)?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn clear_host_mouse_reporting<W: Write>(_writer: &mut W) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    writer: &mut W,
    report_all_keys: bool,
) -> io::Result<()> {
    let mut flags = crate::input::ime_compatible_keyboard_enhancement_flags();
    if report_all_keys {
        flags |= crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    write!(writer, "\x1b[={}u", flags.bits())?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    _writer: &mut W,
    _report_all_keys: bool,
) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_keyboard_report_all_only_changes_the_current_herdr_stack_entry() {
        let mut output = Vec::new();

        set_host_kitty_keyboard_report_all(&mut output, true).unwrap();
        set_host_kitty_keyboard_report_all(&mut output, false).unwrap();

        assert_eq!(output, b"\x1b[=15u\x1b[=7u");
    }

    #[test]
    fn clears_all_known_host_mouse_modes() {
        let sequence = std::str::from_utf8(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE).unwrap();

        for mode in ["1000", "1002", "1003", "1005", "1006", "1015", "1016"] {
            assert!(
                sequence.contains(&format!("\x1b[?{mode}l")),
                "missing mouse mode {mode}"
            );
        }
    }
}
