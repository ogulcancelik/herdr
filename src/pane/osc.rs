use std::borrow::Cow;

use tracing::info;

use crate::layout::PaneId;

use super::terminal::GhosttyPaneCore;

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
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::Ground;
            }
        }

        saw_default_color_set
    }
}

fn is_default_color_set_osc(body: &[u8]) -> bool {
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let command = &body[..separator];
    let value = &body[separator + 1..];
    matches!(command, b"10" | b"11") && !value.is_empty() && value != b"?"
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
    if let Some(color) = theme.foreground {
        let sequence = crate::terminal_theme::osc_set_default_color_sequence(
            crate::terminal_theme::DefaultColorKind::Foreground,
            color,
        );
        terminal.write(sequence.as_bytes());
    }
    if let Some(color) = theme.background {
        let sequence = crate::terminal_theme::osc_set_default_color_sequence(
            crate::terminal_theme::DefaultColorKind::Background,
            color,
        );
        terminal.write(sequence.as_bytes());
    }
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
                cmdline: Some("zsh".to_string()),
            }],
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
