//! One-time tip for Windows Terminal users about its redundant native
//! scrollbar.
//!
//! Windows Terminal (including when herdr runs as a Linux binary under WSL)
//! reserves a column at the window edge for its own scrollbar. herdr draws its
//! own scrollbar, so that column is wasted, and there is no escape sequence to
//! toggle the terminal's native scrollbar from the app side — it is a profile
//! setting only. The best herdr can do is tell the user how to reclaim the
//! column, once, so we don't nag on every launch.

use std::path::{Path, PathBuf};

const SEEN_MARKER: &str = "windows-terminal-scrollbar-hint-seen";

/// Prints the scrollbar tip to stderr the first time herdr exits under Windows
/// Terminal. Call after the terminal has been restored to the normal screen.
pub fn print_scrollbar_hint_once() {
    if let Some(message) = take_scrollbar_hint(is_windows_terminal(), &marker_path()) {
        eprintln!("{message}");
    }
}

fn is_windows_terminal() -> bool {
    std::env::var_os("WT_SESSION").is_some()
}

fn marker_path() -> PathBuf {
    crate::config::state_dir().join(SEEN_MARKER)
}

/// Returns the tip, marking it seen, or `None` when it shouldn't be shown.
///
/// Only records "seen" when we actually return the tip, and treats a failed
/// write as "skip this time" rather than risk repeating it forever.
fn take_scrollbar_hint(under_windows_terminal: bool, marker: &Path) -> Option<String> {
    if !under_windows_terminal || marker.exists() {
        return None;
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::write(marker, b"").ok()?;
    Some(
        "herdr tip: Windows Terminal reserves a column for its native scrollbar, \
         which herdr already draws itself.\n\
         Set \"scrollbarState\": \"hidden\" in your Windows Terminal profile to reclaim it. \
         (Shown only once.)"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_marker(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-wt-hint-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn shows_once_then_marks_seen() {
        let marker = temp_marker("once");
        assert!(!marker.exists());

        let first = take_scrollbar_hint(true, &marker);
        assert!(first.is_some());
        assert!(marker.exists(), "marker should be written after showing");

        // Second run: marker exists, so no tip.
        assert_eq!(take_scrollbar_hint(true, &marker), None);

        let _ = std::fs::remove_file(marker);
    }

    #[test]
    fn skips_outside_windows_terminal() {
        let marker = temp_marker("not-wt");
        assert_eq!(take_scrollbar_hint(false, &marker), None);
        assert!(!marker.exists(), "must not write marker when not shown");
    }
}
