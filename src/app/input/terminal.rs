use bytes::Bytes;
use crossterm::event::KeyCode;
use tracing::{debug, warn};

use crate::{
    app::{App, Mode},
    input::TerminalKey,
};

struct PreparedPaneInput {
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    bytes: Bytes,
}

fn is_modifier_only_key(code: &KeyCode) -> bool {
    matches!(code, KeyCode::Modifier(_))
}

impl App {
    pub(crate) fn handle_terminal_key_headless(&mut self, key: TerminalKey) {
        let Some(input) = self.prepare_terminal_key_forward(key) else {
            return;
        };
        if let Some(runtime) = self.lookup_runtime_sender(input.ws_idx, input.pane_id) {
            let _ = runtime.try_send_bytes(input.bytes);
        }
    }

    fn prepare_terminal_key_forward(&mut self, key: TerminalKey) -> Option<PreparedPaneInput> {
        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;

        let key_event = key.as_key_event();

        if let Some(action) = super::terminal_direct_navigation_action(&self.state, key) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                action = ?action,
                "intercepted terminal direct keybinding before forwarding to pane"
            );
            if action == super::navigate::NavigateAction::EditScrollback {
                self.launch_focused_scrollback_editor();
            } else {
                super::navigate::execute_navigate_action_in_context(
                    &mut self.state,
                    &mut self.terminal_runtimes,
                    action,
                    super::navigate::ActionContext::Direct,
                );
            }
            return None;
        }

        if let Some(binding) = super::navigate::command_for_key(
            &self.state,
            key,
            super::navigate::BindingDispatch::Direct,
        ) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                command = %binding.label,
                "intercepted terminal direct custom command before forwarding to pane"
            );
            self.launch_custom_command(binding, super::navigate::ActionContext::Direct);
            return None;
        }

        if self.state.is_prefix_key(key) {
            self.state.mode = Mode::Prefix;
            return None;
        }

        if is_modifier_only_key(&key_event.code) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                "dropping modifier-only terminal key event instead of forwarding it to pane"
            );
            return None;
        }

        let ws_idx = self.state.active?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        let rt =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;

        // Intercept plain PageUp/PageDown presses for pane scrollback when the
        // focused pane doesn't handle its own scrolling (e.g., a plain shell
        // with mouse off). Modified page keys are pane shortcuts, and release
        // events should not produce a second host-scroll action.
        // Only intercept when we know the pane state; if input_state is unknown,
        // fail-open and forward the key to the pane.
        if matches!(key_event.code, KeyCode::PageUp | KeyCode::PageDown)
            && key_event.modifiers.is_empty()
        {
            if let Some(input_state) = rt.input_state() {
                if !input_state.alternate_screen && !input_state.mouse_reporting_enabled() {
                    if key_event.kind == crossterm::event::KeyEventKind::Release {
                        return None;
                    }
                    if matches!(
                        key_event.kind,
                        crossterm::event::KeyEventKind::Press
                            | crossterm::event::KeyEventKind::Repeat
                    ) {
                        let lines = self
                            .state
                            .pane_info_by_id(pane_id)
                            .map(|info| info.inner_rect.height as usize)
                            .unwrap_or(10)
                            .max(1);
                        if key_event.code == KeyCode::PageUp {
                            self.state
                                .scroll_pane_up(&self.terminal_runtimes, pane_id, lines);
                        } else {
                            self.state
                                .scroll_pane_down(&self.terminal_runtimes, pane_id, lines);
                        }
                        debug!(
                            code = ?key_event.code,
                            lines,
                            "intercepted page key for pane scrollback"
                        );
                        return None;
                    }
                }
            }
        }

        rt.scroll_reset();
        let protocol = rt.keyboard_protocol();
        let bytes = rt.encode_terminal_key(key);

        if matches!(key_event.code, KeyCode::Esc)
            || key_event
                .modifiers
                .contains(crossterm::event::KeyModifiers::ALT)
        {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                protocol = ?protocol,
                encoded = ?bytes,
                "forwarding potentially-ambiguous terminal key to pane"
            );
        }

        if bytes.is_empty() {
            if key.kind != crossterm::event::KeyEventKind::Release
                && !matches!(
                    key.code,
                    KeyCode::CapsLock
                        | KeyCode::ScrollLock
                        | KeyCode::NumLock
                        | KeyCode::PrintScreen
                        | KeyCode::Pause
                        | KeyCode::Menu
                        | KeyCode::KeypadBegin
                        | KeyCode::Media(_)
                        | KeyCode::Modifier(_)
                )
            {
                warn!(code = ?key_event.code, mods = ?key_event.modifiers, state = ?key_event.state, "key produced empty encoding");
            }
            return None;
        }

        Some(PreparedPaneInput {
            ws_idx,
            pane_id,
            bytes: Bytes::from(bytes),
        })
    }

    pub(super) async fn handle_terminal_key(&mut self, key: TerminalKey) {
        let Some(input) = self.prepare_terminal_key_forward(key) else {
            return;
        };
        if let Some(runtime) = self.lookup_runtime_sender(input.ws_idx, input.pane_id) {
            let _ = runtime.send_bytes(input.bytes).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{
        app_for_mouse_test, mouse, numbered_lines_bytes, unique_temp_path, wait_for_file,
    };
    use super::*;
    use crate::{config::Config, events::AppEvent, workspace::Workspace};

    fn app_with_screen_bytes(bytes: &[u8]) -> (App, crate::layout::PaneInfo) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                bytes,
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        (app, info)
    }

    fn double_click(app: &mut App, col: u16, row: u16) {
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    }

    fn modified_mouse(
        kind: MouseEventKind,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column: col,
            row,
            modifiers,
        }
    }

    fn clipboard_write_content(app: &mut App) -> Vec<u8> {
        match app.event_rx.try_recv().expect("clipboard write event") {
            AppEvent::ClipboardWrite { content } => content,
            event => panic!("unexpected event: {event:?}"),
        }
    }

    fn assert_visible_selection(app: &App) {
        assert!(app
            .state
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_visible));
    }

    #[tokio::test]
    async fn dragging_selection_above_pane_autoscrolls_and_extends_into_scrollback() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let start_metrics = app
            .state
            .runtime_for_pane(&app.terminal_runtimes, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("initial scroll metrics");
        let start_row = info.inner_rect.y;
        let start_col = info.inner_rect.x + 2;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            start_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            start_col,
            info.inner_rect.y.saturating_sub(1),
        ));

        let end_metrics = app
            .state
            .runtime_for_pane(&app.terminal_runtimes, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after drag");
        assert_eq!(
            end_metrics.offset_from_bottom,
            start_metrics.offset_from_bottom + 3
        );

        let selection = app.state.selection.as_ref().expect("selection after drag");
        assert!(selection.is_visible());
        assert_eq!(
            selection.ordered_cells(),
            (
                (
                    (start_metrics.max_offset_from_bottom - end_metrics.offset_from_bottom) as u32,
                    2,
                ),
                (start_metrics.max_offset_from_bottom as u32, 2),
            )
        );
    }

    #[tokio::test]
    async fn releasing_dragged_selection_clears_highlight_after_copy() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let row = info.inner_rect.y;
        let start_col = info.inner_rect.x + 1;
        let end_col = info.inner_rect.x + 4;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        assert!(app.state.selection.is_some());

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

        assert!(app.state.selection.is_none());
    }

    #[tokio::test]
    async fn drag_copy_then_click_does_not_reuse_double_click_candidate() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        let row = info.inner_rect.y;
        let start_col = info.inner_rect.x;
        let end_col = info.inner_rect.x + 4;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));
        assert!(app.last_pane_click.is_some());

        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        assert!(app.last_pane_click.is_none());

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));
        assert!(app.last_pane_click.is_none());
        assert_eq!(clipboard_write_content(&mut app), b"alpha");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));

        assert!(app.last_pane_click.is_some());
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn double_click_selects_and_copies_word() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta-gamma_delta@omega");
        let col = info.inner_rect.x + 13;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);

        assert_eq!(clipboard_write_content(&mut app), b"beta-gamma_delta@omega");
        assert_visible_selection(&app);
    }

    #[tokio::test]
    async fn double_click_uses_display_columns_for_wide_chars() {
        let (mut app, info) = app_with_screen_bytes("echo 你好-world done".as_bytes());
        let col = info.inner_rect.x + 8;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);

        assert_eq!(clipboard_write_content(&mut app), "你好-world".as_bytes());
        assert_visible_selection(&app);
    }

    #[tokio::test]
    async fn double_click_copies_quoted_path_without_quotes() {
        let line = r#"cat "/tmp/build output/log.txt""#;
        let (mut app, info) = app_with_screen_bytes(line.as_bytes());
        let col = info.inner_rect.x + line.find("output").expect("path segment") as u16;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);

        assert_eq!(
            clipboard_write_content(&mut app),
            b"/tmp/build output/log.txt"
        );
        assert_visible_selection(&app);
    }

    #[tokio::test]
    async fn double_click_excludes_trailing_punctuation() {
        let (mut app, info) = app_with_screen_bytes(b"done.");
        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);

        assert_eq!(clipboard_write_content(&mut app), b"done");
        assert_visible_selection(&app);
    }

    #[tokio::test]
    async fn modified_pane_click_does_not_seed_double_click_copy() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        let col = info.inner_rect.x + 7;
        let row = info.inner_rect.y;

        app.handle_mouse(modified_mouse(
            MouseEventKind::Down(MouseButton::Left),
            col,
            row,
            KeyModifiers::CONTROL,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert!(app.event_rx.try_recv().is_err());
        assert!(app.selection_highlight_clear_deadline.is_none());
    }

    #[tokio::test]
    async fn pane_cell_url_resolver_finds_visible_url() {
        let line = "see https://example.com/pr/307.";
        let (app, info) = app_with_screen_bytes(line.as_bytes());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let col = line.find("example").expect("url host") as u16;

        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 0, col)
                .as_deref(),
            Some("https://example.com/pr/307")
        );
        assert_eq!(
            app.state.url_at_pane_cell(
                &app.terminal_runtimes,
                pane_id,
                0,
                info.inner_rect.width - 1
            ),
            None
        );
    }

    #[tokio::test]
    async fn pane_cell_url_resolver_prefers_osc8_hyperlink() {
        let (app, _info) = app_with_screen_bytes(
            b"\x1b]8;;https://example.com/hidden-target\x1b\\label\x1b]8;;\x1b\\",
        );
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 0, 1)
                .as_deref(),
            Some("https://example.com/hidden-target")
        );
    }

    #[tokio::test]
    async fn double_click_highlight_clears_after_short_delay() {
        let (mut app, info) = app_with_screen_bytes(b"copied");
        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);
        assert_eq!(clipboard_write_content(&mut app), b"copied");

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));

        assert!(app.event_rx.try_recv().is_err());
        assert!(app.state.selection.is_some());
        let deadline = app
            .selection_highlight_clear_deadline
            .expect("highlight clear deadline");
        assert!(app.handle_scheduled_tasks(deadline + std::time::Duration::from_millis(1), false));
        assert!(app.state.selection.is_none());
    }

    #[tokio::test]
    async fn double_click_is_forwarded_when_mouse_reporting_is_enabled() {
        let (mut app, info) = app_with_screen_bytes(b"\x1b[?1002halpha beta");
        let col = info.inner_rect.x + 8;
        let row = info.inner_rect.y;
        double_click(&mut app, col, row);

        assert!(app.event_rx.try_recv().is_err());
        assert!(app.state.selection.is_none());
        assert!(app.selection_highlight_clear_deadline.is_none());
    }

    #[tokio::test]
    async fn wheel_scroll_keeps_in_progress_selection_and_extends_it() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let start_metrics = app
            .state
            .runtime_for_pane(&app.terminal_runtimes, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("initial scroll metrics");
        let top_row = info.inner_rect.y;
        let col = info.inner_rect.x + 2;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, top_row));
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, col, top_row));

        let end_metrics = app
            .state
            .runtime_for_pane(&app.terminal_runtimes, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after wheel");
        assert_eq!(
            end_metrics.offset_from_bottom,
            start_metrics.offset_from_bottom + 3
        );

        let selection = app.state.selection.as_ref().expect("selection after wheel");
        assert!(selection.is_visible());
        assert_eq!(
            selection.ordered_cells(),
            (
                (
                    (start_metrics.max_offset_from_bottom - end_metrics.offset_from_bottom) as u32,
                    2,
                ),
                (start_metrics.max_offset_from_bottom as u32, 2),
            )
        );
    }

    #[tokio::test]
    async fn clicking_unfocused_pane_with_mouse_reporting_focuses_it_via_left_button() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Vertical);

        let terminal_area = Rect::new(26, 2, 80, 18);
        let pane_infos = ws.tabs[0].layout.panes(terminal_area);
        let first_info = pane_infos
            .iter()
            .find(|p| p.id == first_pane)
            .unwrap()
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|p| p.id == second_pane)
            .unwrap()
            .clone();

        ws.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"",
            ),
        );
        ws.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        ws.tabs[0].layout.focus_pane(first_pane);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second_info.inner_rect.x + 2,
            second_info.inner_rect.y + 2,
        ));

        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn clicking_unfocused_pane_with_mouse_reporting_focuses_it_via_right_button() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Vertical);

        let terminal_area = Rect::new(26, 2, 80, 18);
        let pane_infos = ws.tabs[0].layout.panes(terminal_area);
        let first_info = pane_infos
            .iter()
            .find(|p| p.id == first_pane)
            .unwrap()
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|p| p.id == second_pane)
            .unwrap()
            .clone();

        ws.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"",
            ),
        );
        ws.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        ws.tabs[0].layout.focus_pane(first_pane);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_info.inner_rect.x + 2,
            second_info.inner_rect.y + 2,
        ));

        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
    }

    #[tokio::test]
    async fn terminal_direct_focus_pane_shortcut_switches_focus_without_leaving_terminal_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(Rect::new(0, 0, 80, 24));
        let focused_before = app.state.workspaces[0].layout.focused();
        app.state.keybinds.focus_pane_left = crate::config::ActionKeybinds::direct("alt+h");

        app.handle_terminal_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::ALT))
            .await;

        assert_ne!(app.state.workspaces[0].layout.focused(), focused_before);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn terminal_direct_edit_scrollback_opens_editor_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                5,
                4096,
                b"alpha\nbeta\n",
            ),
        );
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("direct-edit-scrollback");
        let previous_editor = std::env::var_os("EDITOR");
        std::env::set_var(
            "EDITOR",
            format!("sh -c 'cp \"$1\" {}' sh", output_path.display()),
        );
        app.state.keybinds.edit_scrollback = crate::config::ActionKeybinds::direct("ctrl+alt+e");

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        match previous_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        let content = wait_for_file(&output_path);
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert_eq!(app.state.mode, Mode::Terminal);

        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn direct_custom_command_runs_before_forwarding_to_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("direct-custom-command");
        let command = format!("printf direct > '{}'", output_path.display());
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::direct("ctrl+alt+g"),
            label: "ctrl+alt+g".into(),
            command,
            action: crate::config::CustomCommandAction::Shell,
            description: None,
        }];

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        assert_eq!(wait_for_file(&output_path), "direct");
        assert_eq!(app.state.mode, Mode::Terminal);
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn direct_custom_pane_command_opens_overlay_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = "/usr/bin/true".into();
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.host_terminal_theme,
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::direct("ctrl+alt+g"),
            label: "ctrl+alt+g".into(),
            command: "printf direct-pane".into(),
            action: crate::config::CustomCommandAction::Pane,
            description: None,
        }];

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.mode, Mode::Terminal);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn alt_backspace_is_forwarded_to_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(0, 0, 80, 24));
        let info = pane_infos[0].clone();
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(
            info.inner_rect.width,
            info.inner_rect.height,
        );
        ws.tabs[0].runtimes.insert(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let key = crate::input::parse_terminal_key_sequence("\x1b\x7f").unwrap();
        app.handle_terminal_key_headless(key);

        let bytes = rx.try_recv().unwrap();
        assert_eq!(bytes.as_ref(), b"\x1b\x7f");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn page_up_scrolls_plain_shell_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let start_metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("initial scroll metrics");
        assert_eq!(start_metrics.offset_from_bottom, 0);

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));

        let end_metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp");
        assert_eq!(
            end_metrics.offset_from_bottom,
            info.inner_rect.height as usize
        );
    }

    #[tokio::test]
    async fn page_down_returns_to_bottom_after_page_up() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        let after_up = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp");
        assert!(after_up.offset_from_bottom > 0);

        app.handle_terminal_key_headless(TerminalKey::new(
            KeyCode::PageDown,
            KeyModifiers::empty(),
        ));
        let after_down = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageDown");
        assert_eq!(after_down.offset_from_bottom, 0);
    }

    #[tokio::test]
    async fn page_up_release_does_not_scroll_plain_shell_pane_again() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        let after_press = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp press");
        assert_eq!(
            after_press.offset_from_bottom,
            info.inner_rect.height as usize
        );

        app.handle_terminal_key_headless(
            TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty())
                .with_kind(KeyEventKind::Release),
        );

        let after_release = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp release");
        assert_eq!(
            after_release.offset_from_bottom,
            after_press.offset_from_bottom
        );
    }

    #[tokio::test]
    async fn modified_page_up_does_not_host_scroll_plain_shell_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::PageUp, KeyModifiers::CONTROL));

        let metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after modified PageUp");
        assert_eq!(metrics.offset_from_bottom, 0);
    }

    #[tokio::test]
    async fn page_up_forwarded_to_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let mut bytes = b"\x1b[?1002h".to_vec();
        bytes.extend_from_slice(&numbered_lines_bytes(64));
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &bytes,
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let start_metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("initial scroll metrics");
        assert_eq!(start_metrics.offset_from_bottom, 0);

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));

        let end_metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp");
        // Forwarded to pane, so test runtime doesn't process it — scroll stays at bottom.
        assert_eq!(end_metrics.offset_from_bottom, 0);
    }
}
