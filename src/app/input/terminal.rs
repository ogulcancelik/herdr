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
        self.state.update_dismissed = true;

        let key_event = key.as_key_event();

        if let Some(action) = super::terminal_direct_navigation_action(&self.state, &key_event) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                action = ?action,
                "intercepted terminal direct navigation key before forwarding to pane"
            );
            super::navigate::execute_navigate_action(&mut self.state, action);
            return None;
        }

        if self.state.is_prefix(&key_event) {
            self.state.mode = Mode::Navigate;
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
        let rt = ws.runtimes.get(&pane_id)?;
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
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse, numbered_lines_bytes};
    use super::*;
    use crate::{config::Config, workspace::Workspace};

    #[tokio::test]
    async fn dragging_selection_above_pane_autoscrolls_and_extends_into_scrollback() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::pane::PaneRuntime::test_with_scrollback_bytes(
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

        let start_metrics = app.state.workspaces[0]
            .runtime(pane_id)
            .and_then(crate::pane::PaneRuntime::scroll_metrics)
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

        let end_metrics = app.state.workspaces[0]
            .runtime(pane_id)
            .and_then(crate::pane::PaneRuntime::scroll_metrics)
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
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::pane::PaneRuntime::test_with_scrollback_bytes(
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
    async fn wheel_scroll_keeps_in_progress_selection_and_extends_it() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::pane::PaneRuntime::test_with_scrollback_bytes(
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

        let start_metrics = app.state.workspaces[0]
            .runtime(pane_id)
            .and_then(crate::pane::PaneRuntime::scroll_metrics)
            .expect("initial scroll metrics");
        let top_row = info.inner_rect.y;
        let col = info.inner_rect.x + 2;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, top_row));
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, col, top_row));

        let end_metrics = app.state.workspaces[0]
            .runtime(pane_id)
            .and_then(crate::pane::PaneRuntime::scroll_metrics)
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

        ws.tabs[0].runtimes.insert(
            first_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"",
            ),
        );
        ws.tabs[0].runtimes.insert(
            second_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
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

        ws.tabs[0].runtimes.insert(
            first_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"",
            ),
        );
        ws.tabs[0].runtimes.insert(
            second_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
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
        app.state.keybinds.focus_pane_left = Some((KeyCode::Char('h'), KeyModifiers::ALT));
        app.state.keybinds.focus_pane_left_label = Some("alt+h".into());

        app.handle_terminal_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::ALT))
            .await;

        assert_ne!(app.state.workspaces[0].layout.focused(), focused_before);
        assert_eq!(app.state.mode, Mode::Terminal);
    }
}
