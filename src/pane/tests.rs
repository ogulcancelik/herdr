use super::*;
use ratatui::style::Color;

use super::terminal::{
    ghostty_normalize_buffer_symbol, trim_trailing_blank_rows, GhosttyPaneTerminal,
};

impl PaneRuntime {
    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[])
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self::test_with_scrollback_bytes(cols, rows, 0, bytes)
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, scrollback_limit_bytes, bytes).0
    }

    fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let mut terminal =
            crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes).unwrap();
        terminal.write(bytes);

        (
            Self {
                terminal: Arc::new(PaneTerminal::new(
                    GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
                )),
                sender: tx,
                resize_tx,
                current_size: Cell::new((rows, cols)),
                child_pid: Arc::new(AtomicU32::new(0)),
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                detect_reset_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                detect_handle: tokio::spawn(async {}).abort_handle(),
            },
            rx,
        )
    }
}

fn write_numbered_lines(terminal: &mut crate::ghostty::Terminal, count: usize) {
    for i in 0..count {
        terminal.write(format!("{i:06}\r\n").as_bytes());
    }
}

#[test]
fn ghostty_render_can_suppress_cursor_position() {
    let (tx, _rx) = mpsc::channel(4);
    let mut first_terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
    first_terminal.write(b"left");
    let first = GhosttyPaneTerminal::new(first_terminal, tx.clone()).unwrap();

    let mut second_terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
    second_terminal.write(b"r\r\nb");
    let second = GhosttyPaneTerminal::new(second_terminal, tx).unwrap();

    let backend = ratatui::backend::TestBackend::new(40, 5);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            first.render(frame, Rect::new(0, 0, 20, 5), true);
            second.render(frame, Rect::new(20, 0, 20, 5), false);
        })
        .unwrap();

    terminal.backend_mut().assert_cursor_position((4, 0));
}

#[test]
fn ghostty_keyboard_protocol_tracks_live_terminal_flags() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal.write(b"\x1b[>3u");
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    assert_eq!(
        pane.keyboard_protocol(),
        Some(crate::input::KeyboardProtocol::Kitty { flags: 3 })
    );
}

#[test]
fn ghostty_plain_text_chars_still_encode_as_text() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let encoded = pane.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );

    assert_eq!(encoded, b"a");
}

#[test]
fn ghostty_char_keys_still_use_herdr_encoding() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal.write(b"\x1b[>1u");
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let encoded = pane.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        ),
        crate::input::KeyboardProtocol::Legacy,
    );

    assert_eq!(encoded, vec![1]);
}

#[test]
fn ghostty_key_encoding_honors_application_cursor_mode() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal
        .mode_set(crate::ghostty::MODE_APPLICATION_CURSOR_KEYS, true)
        .unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let encoded = pane.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );

    assert_eq!(encoded, b"\x1bOA");
}

#[test]
fn ghostty_key_encoder_updates_after_terminal_mode_changes() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
    let pane_id = PaneId::from_raw(1);

    let before = pane.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );
    assert_eq!(before, b"\x1b[A");

    pane.process_pty_bytes(pane_id, 0, b"\x1b[?1h", &tx);

    let after = pane.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );
    assert_eq!(after, b"\x1bOA");
}

#[test]
fn ghostty_key_encoder_updates_after_kitty_flag_changes() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
    let pane_id = PaneId::from_raw(1);
    let key = crate::input::TerminalKey::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
    );

    let before = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);
    pane.process_pty_bytes(pane_id, 0, b"\x1b[>1u", &tx);
    let after = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

    assert_ne!(before, after);
    assert_eq!(after, b"\x1b[13;6u");
}

#[test]
fn ghostty_key_encoders_are_isolated_per_pane() {
    let (tx, _rx) = mpsc::channel(4);
    let first = GhosttyPaneTerminal::new(
        crate::ghostty::Terminal::new(80, 24, 0).unwrap(),
        tx.clone(),
    )
    .unwrap();
    let second = GhosttyPaneTerminal::new(
        crate::ghostty::Terminal::new(80, 24, 0).unwrap(),
        tx.clone(),
    )
    .unwrap();

    first.process_pty_bytes(PaneId::from_raw(1), 0, b"\x1b[?1h", &tx);

    let first_encoded = first.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );
    let second_encoded = second.encode_terminal_key(
        crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::empty(),
        ),
        crate::input::KeyboardProtocol::Legacy,
    );

    assert_eq!(first_encoded, b"\x1bOA");
    assert_eq!(second_encoded, b"\x1b[A");
}

#[test]
fn ghostty_mouse_button_encoding_uses_live_terminal_state() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal.write(b"\x1b[?1000h\x1b[?1006h");
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let encoded = pane.encode_mouse_button(
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        11,
        9,
        crossterm::event::KeyModifiers::empty(),
    );

    assert_eq!(encoded.as_deref(), Some(&b"\x1b[<0;12;10m"[..]));
}

#[test]
fn ghostty_mouse_drag_encoding_uses_motion_reporting_state() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal.write(b"\x1b[?1002h\x1b[?1006h");
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let encoded = pane.encode_mouse_button(
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        4,
        6,
        crossterm::event::KeyModifiers::SHIFT,
    );

    assert_eq!(encoded.as_deref(), Some(&b"\x1b[<36;5;7M"[..]));
}

#[test]
fn ghostty_normalize_buffer_symbol_prefers_grapheme_width_when_metadata_disagrees() {
    const WIDE_GRAPHEME: &str = "🙂";
    const VS16_GRAPHEME: &str = "⚠️";
    const EMOJI_GRAPHEME: &str = "💳";

    assert_eq!(
        ghostty_normalize_buffer_symbol(WIDE_GRAPHEME, crate::ghostty::CellWide::Wide),
        WIDE_GRAPHEME
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol("a", crate::ghostty::CellWide::Wide),
        "  "
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol("⌨️", crate::ghostty::CellWide::Narrow),
        "⌨️"
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol(VS16_GRAPHEME, crate::ghostty::CellWide::Narrow),
        VS16_GRAPHEME
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol(EMOJI_GRAPHEME, crate::ghostty::CellWide::Narrow),
        EMOJI_GRAPHEME
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol(" ", crate::ghostty::CellWide::SpacerTail),
        ""
    );
    assert_eq!(
        ghostty_normalize_buffer_symbol("xx", crate::ghostty::CellWide::SpacerHead),
        " "
    );
}

#[test]
fn pane_scrollback_controls_reach_top_without_ui_interference() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 3, 100).unwrap();
    write_numbered_lines(&mut terminal, 1000);
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let before = pane.scroll_metrics().expect("scroll metrics before scroll");
    assert!(before.max_offset_from_bottom > 0);
    assert_eq!(before.offset_from_bottom, 0);

    pane.set_scroll_offset_from_bottom(before.max_offset_from_bottom);

    let after = pane.scroll_metrics().expect("scroll metrics after scroll");
    assert_eq!(after.offset_from_bottom, after.max_offset_from_bottom);
    assert!(pane.visible_text().contains("000000"));
}

#[test]
fn detection_text_stays_at_bottom_when_viewport_is_scrolled() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(80, 3, 100).unwrap();
    write_numbered_lines(&mut terminal, 10);
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    let bottom_snapshot = pane.detection_text();
    assert_eq!(bottom_snapshot, pane.recent_text(3));
    assert!(bottom_snapshot.contains("000009"));

    let before = pane.scroll_metrics().expect("scroll metrics before scroll");
    pane.set_scroll_offset_from_bottom(before.max_offset_from_bottom);

    assert!(pane.visible_text().contains("000000"));
    assert_eq!(pane.detection_text(), bottom_snapshot);
}

#[test]
fn extract_selection_reads_screen_rows_not_current_viewport() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(8, 3, 1024).unwrap();
    write_numbered_lines(&mut terminal, 8);
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    pane.set_scroll_offset_from_bottom(3);
    let metrics = pane
        .scroll_metrics()
        .expect("scroll metrics after initial scroll");
    let mut selection =
        crate::selection::Selection::anchor(PaneId::from_raw(1), 0, 0, Some(metrics));
    selection.drag(5, 2, Rect::new(0, 0, 8, 3), Some(metrics));

    pane.scroll_reset();

    let text = pane
        .extract_selection(&selection)
        .expect("selection should extract text");
    assert_eq!(text, "000003\n000004\n000005");
}

#[test]
fn recent_unwrapped_text_ignores_soft_wraps() {
    let (tx, _rx) = mpsc::channel(4);
    let mut terminal = crate::ghostty::Terminal::new(5, 3, 100).unwrap();
    terminal.write(b"ABCDEFGHIJ");
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

    assert_eq!(pane.recent_text(3), "ABCDE\nFGHIJ\n");
    assert_eq!(pane.recent_unwrapped_text(3), "ABCDEFGHIJ");
}

#[test]
fn synchronized_output_suppresses_intermediate_render_requests_until_batch_ends() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    let pane_terminal = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
    let pane_id = PaneId::from_raw(1);

    let begin = pane_terminal.process_pty_bytes(pane_id, 0, b"\x1b[?2026h", &tx);
    assert!(!begin.request_render);

    let body = pane_terminal.process_pty_bytes(pane_id, 0, b"hello", &tx);
    assert!(!body.request_render);

    let end = pane_terminal.process_pty_bytes(pane_id, 0, b"\x1b[?2026l", &tx);
    assert!(end.request_render);
}

#[test]
fn render_leaves_host_default_background_transparent() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
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
        core.terminal.write(b"hi");
    }

    let backend = ratatui::backend::TestBackend::new(20, 5);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
        .unwrap();

    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "h");
    assert_eq!(buffer[(0, 0)].style().bg, Some(Color::Reset));
    assert_eq!(buffer[(2, 0)].symbol(), " ");
    assert_eq!(buffer[(2, 0)].style().bg, Some(Color::Reset));
}

#[test]
fn render_keeps_explicit_default_background_when_it_differs_from_host() {
    let (tx, _rx) = mpsc::channel(4);
    let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
    let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
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
        core.terminal.write(b"\x1b]11;rgb:44/55/66\x1b\\hi");
    }

    let backend = ratatui::backend::TestBackend::new(20, 5);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let expected_bg = Some(Color::Rgb(0x44, 0x55, 0x66));
    assert_eq!(buffer[(0, 0)].symbol(), "h");
    assert_eq!(buffer[(0, 0)].style().bg, expected_bg);
    assert_eq!(buffer[(2, 0)].symbol(), " ");
    assert_eq!(buffer[(2, 0)].style().bg, expected_bg);
}

#[tokio::test]
async fn focus_events_are_forwarded_when_enabled() {
    let (tx, mut rx) = mpsc::channel(4);
    let (resize_tx, _resize_rx) = mpsc::channel(1);
    let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    terminal
        .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
        .unwrap();
    let runtime = PaneRuntime {
        terminal: Arc::new(PaneTerminal::new(
            GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
        )),
        sender: tx,
        resize_tx,
        current_size: Cell::new((80, 24)),
        child_pid: Arc::new(AtomicU32::new(0)),
        kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
        detect_reset_notify: Arc::new(Notify::new()),
        pending_release: Arc::new(Mutex::new(None)),
        detect_handle: tokio::spawn(async {}).abort_handle(),
    };

    assert!(runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
    assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"\x1b[I"));
}

#[tokio::test]
async fn focus_events_are_suppressed_when_disabled() {
    let (tx, mut rx) = mpsc::channel(4);
    let (resize_tx, _resize_rx) = mpsc::channel(1);
    let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
    let runtime = PaneRuntime {
        terminal: Arc::new(PaneTerminal::new(
            GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
        )),
        sender: tx,
        resize_tx,
        current_size: Cell::new((80, 24)),
        child_pid: Arc::new(AtomicU32::new(0)),
        kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
        detect_reset_notify: Arc::new(Notify::new()),
        pending_release: Arc::new(Mutex::new(None)),
        detect_handle: tokio::spawn(async {}).abort_handle(),
    };

    assert!(!runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
            .await
            .is_err()
    );
}

#[test]
fn trim_trailing_blank_rows_drops_empty_viewport_tail() {
    let mut rows = vec!["hello".to_string(), "".to_string(), "   ".to_string()];
    trim_trailing_blank_rows(&mut rows);
    assert_eq!(rows, vec!["hello".to_string()]);
}

#[tokio::test]
async fn state_changed_event_waits_for_queue_space_instead_of_dropping() {
    let (tx, mut rx) = mpsc::channel(1);
    let pane_id = PaneId::from_raw(42);

    tx.try_send(AppEvent::UpdateReady {
        version: "9.9.9".into(),
    })
    .unwrap();

    let publish =
        publish_state_changed_event(tx.clone(), pane_id, Some(Agent::Pi), AgentState::Idle);
    tokio::pin!(publish);

    let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), async {
        (&mut publish).await;
    })
    .await;
    assert!(
        blocked.is_err(),
        "publisher should wait for queue space instead of dropping StateChanged"
    );

    let first = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
        .await
        .expect("queue should yield first event")
        .expect("sender still alive");
    assert!(matches!(first, AppEvent::UpdateReady { .. }));

    tokio::time::timeout(std::time::Duration::from_millis(50), async {
        (&mut publish).await;
    })
    .await
    .expect("publisher should complete once queue space is available");

    let second = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
        .await
        .expect("queue should yield second event")
        .expect("sender still alive");
    assert!(matches!(
        second,
        AppEvent::StateChanged {
            pane_id: delivered_pane,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        } if delivered_pane == pane_id
    ));
}
