use bytes::Bytes;
use crossterm::event::KeyCode;
use tracing::{debug, warn};

use crate::{
    app::{App, Mode, TerminalInputTarget},
    input::TerminalKey,
};

struct PreparedPaneInput {
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    target: TerminalInputTarget,
    bytes: Bytes,
}

enum PreparedPopupInput {
    NotOpen,
    Consumed,
    Bytes {
        target: TerminalInputTarget,
        bytes: Bytes,
    },
}

fn is_modifier_only_key(code: &KeyCode) -> bool {
    matches!(code, KeyCode::Modifier(_))
}

impl App {
    pub(crate) fn handle_terminal_key_headless(
        &mut self,
        key: TerminalKey,
    ) -> Option<TerminalInputTarget> {
        match self.prepare_popup_key_forward(key) {
            PreparedPopupInput::NotOpen => {}
            PreparedPopupInput::Consumed => return None,
            PreparedPopupInput::Bytes { target, bytes } => {
                let Some(runtime) = self.popup_runtime() else {
                    self.close_popup_pane();
                    return None;
                };
                return runtime.try_send_bytes(bytes).is_ok().then_some(target);
            }
        }

        let input = self.prepare_terminal_key_forward(key)?;
        let sent = self
            .lookup_runtime_sender(input.ws_idx, input.pane_id)
            .is_some_and(|runtime| runtime.try_send_bytes(input.bytes).is_ok());
        sent.then_some(input.target)
    }

    fn prepare_terminal_key_forward(&mut self, key: TerminalKey) -> Option<PreparedPaneInput> {
        self.state.clear_selection();
        self.selection_autoscroll_deadline = None;
        self.state.update_dismissed = true;

        let key_event = key.as_key_event();

        if let Some(action) = super::terminal_direct_non_indexed_navigation_action(&self.state, key)
        {
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
                self.execute_tui_navigate_action(action, super::navigate::ActionContext::Direct);
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

        if let Some(action) = super::terminal_direct_indexed_navigation_action(&self.state, key) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                action = ?action,
                "intercepted terminal direct indexed keybinding before forwarding to pane"
            );
            self.execute_tui_navigate_action(action, super::navigate::ActionContext::Direct);
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
        let terminal_id = ws.terminal_id(pane_id)?.clone();
        let rt =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;

        // Intercept plain PageUp/PageDown presses for pane scrollback only
        // when the focused pane looks like a shell transcript. Normal-screen
        // pagers such as `less -X` keep the primary screen but enter
        // application cursor mode while they own special keys.
        // Modified page keys are pane shortcuts, and release events should not
        // produce a second host-scroll action.
        // Only intercept when we know the pane state; if input_state is unknown,
        // fail-open and forward the key to the pane.
        if matches!(key_event.code, KeyCode::PageUp | KeyCode::PageDown)
            && key_event.modifiers.is_empty()
        {
            if let Some(input_state) = rt.input_state() {
                if input_state.plain_page_keys_use_host_scrollback() {
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
            target: TerminalInputTarget { terminal_id },
            bytes: Bytes::from(bytes),
        })
    }

    fn prepare_popup_key_forward(&mut self, key: TerminalKey) -> PreparedPopupInput {
        if self.state.popup_pane.is_none() {
            return PreparedPopupInput::NotOpen;
        }
        let Some(terminal_id) = self
            .state
            .popup_pane
            .as_ref()
            .map(|popup| popup.terminal_id.clone())
        else {
            return PreparedPopupInput::NotOpen;
        };
        let Some(rt) = self.terminal_runtimes.get(&terminal_id) else {
            self.close_popup_pane();
            return PreparedPopupInput::Consumed;
        };
        rt.scroll_reset();
        let bytes = rt.encode_terminal_key(key);
        self.state.mode = Mode::Terminal;
        if bytes.is_empty() {
            PreparedPopupInput::Consumed
        } else {
            PreparedPopupInput::Bytes {
                target: TerminalInputTarget { terminal_id },
                bytes: Bytes::from(bytes),
            }
        }
    }

    pub(crate) fn host_keyboard_report_all_requested(&self) -> bool {
        let runtime = if self.state.popup_pane.is_some() {
            self.popup_runtime()
        } else if self.state.mode == Mode::Terminal {
            self.state.active.and_then(|ws_idx| {
                self.state
                    .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
            })
        } else {
            None
        };

        runtime.is_some_and(|runtime| runtime.keyboard_protocol().reports_all_keys())
    }

    fn terminal_input_runtime(
        &self,
        target: &TerminalInputTarget,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        if let Some(runtime) = self.terminal_runtimes.get(&target.terminal_id) {
            return Some(runtime);
        }
        #[cfg(test)]
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (&pane_id, pane) in &tab.panes {
                    if pane.attached_terminal_id == target.terminal_id {
                        return self.state.runtime_for_pane_in_workspace(
                            &self.terminal_runtimes,
                            ws_idx,
                            pane_id,
                        );
                    }
                }
            }
        }
        None
    }

    pub(crate) fn forward_terminal_key_to_target_headless(
        &self,
        target: &TerminalInputTarget,
        key: TerminalKey,
    ) -> bool {
        let Some(runtime) = self.terminal_input_runtime(target) else {
            return false;
        };
        let bytes = runtime.encode_terminal_key(key);
        bytes.is_empty() || runtime.try_send_bytes(Bytes::from(bytes)).is_ok()
    }

    pub(crate) async fn forward_terminal_key_to_target(
        &self,
        target: &TerminalInputTarget,
        key: TerminalKey,
    ) -> bool {
        let Some(runtime) = self.terminal_input_runtime(target) else {
            return false;
        };
        let bytes = runtime.encode_terminal_key(key);
        bytes.is_empty() || runtime.send_bytes(Bytes::from(bytes)).await.is_ok()
    }

    fn take_pressed_keys_for_source(
        &mut self,
        source_id: crate::app::InputSourceId,
    ) -> Vec<crate::app::PressedTerminalKey> {
        let pressed = self
            .pressed_terminal_keys
            .iter()
            .filter(|((id, _), _)| *id == source_id)
            .map(|(_, pressed)| pressed.clone())
            .collect();
        self.pressed_terminal_keys
            .retain(|(id, _), _| *id != source_id);
        self.suppressed_repeat_keys
            .retain(|(id, _)| *id != source_id);
        pressed
    }

    pub(crate) fn release_input_source_headless(&mut self, source_id: crate::app::InputSourceId) {
        for pressed in self.take_pressed_keys_for_source(source_id) {
            let release = pressed
                .key
                .with_kind(crossterm::event::KeyEventKind::Release);
            let _ = self.forward_terminal_key_to_target_headless(&pressed.target, release);
        }
    }

    pub(crate) async fn release_input_source(&mut self, source_id: crate::app::InputSourceId) {
        for pressed in self.take_pressed_keys_for_source(source_id) {
            let release = pressed
                .key
                .with_kind(crossterm::event::KeyEventKind::Release);
            let _ = self
                .forward_terminal_key_to_target(&pressed.target, release)
                .await;
        }
    }

    pub(super) async fn handle_terminal_key(
        &mut self,
        key: TerminalKey,
    ) -> Option<TerminalInputTarget> {
        match self.prepare_popup_key_forward(key) {
            PreparedPopupInput::NotOpen => {}
            PreparedPopupInput::Consumed => return None,
            PreparedPopupInput::Bytes { target, bytes } => {
                let Some(runtime) = self.popup_runtime() else {
                    self.close_popup_pane();
                    return None;
                };
                return runtime.send_bytes(bytes).await.is_ok().then_some(target);
            }
        }

        let input = self.prepare_terminal_key_forward(key)?;
        let sent = if let Some(runtime) = self.lookup_runtime_sender(input.ws_idx, input.pane_id) {
            runtime.send_bytes(input.bytes).await.is_ok()
        } else {
            false
        };
        sent.then_some(input.target)
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse, numbered_lines_bytes};
    #[cfg(unix)]
    use super::super::{unique_temp_path, wait_for_file};
    use super::*;
    use crate::{config::Config, events::AppEvent, workspace::Workspace};

    #[cfg(unix)]
    fn app_with_spawned_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = "/bin/sh".into();
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
        app
    }

    #[cfg(unix)]
    fn shutdown_test_runtimes(app: &mut App) {
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

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

    #[cfg(unix)]
    fn install_test_link_handler(app: &mut App) {
        let plugin_root = std::env::temp_dir();
        app.state.installed_plugins = std::collections::HashMap::from([(
            "example.links".to_string(),
            crate::api::schema::InstalledPluginInfo {
                plugin_id: "example.links".into(),
                name: "Links".into(),
                version: "0.1.0".into(),
                min_herdr_version: "0.6.10".into(),
                description: None,
                manifest_path: plugin_root.join("herdr-plugin.toml").display().to_string(),
                plugin_root: plugin_root.display().to_string(),
                enabled: true,
                platforms: None,
                build: Vec::new(),
                startup: Vec::new(),
                actions: vec![crate::api::schema::PluginManifestAction {
                    id: "open".into(),
                    title: "Open link".into(),
                    description: None,
                    contexts: Vec::new(),
                    platforms: None,
                    command: vec!["sh".into(), "-c".into(), ":".into()],
                }],
                events: Vec::new(),
                panes: Vec::new(),
                link_handlers: vec![crate::api::schema::PluginManifestLinkHandler {
                    id: "github-issue".into(),
                    title: "Open GitHub issue".into(),
                    pattern: "^https://github\\.com/[^/]+/[^/]+/(issues|pull)/[0-9]+$".into(),
                    action: "open".into(),
                    platforms: None,
                }],
                source: crate::api::schema::PluginSourceInfo::default(),
                warnings: Vec::new(),
            },
        )]);
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
    async fn copy_on_select_disabled_keeps_drag_selection_without_copying() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        app.state.copy_on_select = false;
        let row = info.inner_rect.y;
        let start_col = info.inner_rect.x;
        let end_col = info.inner_rect.x + 4;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        assert_visible_selection(&app);
        assert!(!app
            .state
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_finalized));

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

        assert_visible_selection(&app);
        assert_eq!(
            app.state
                .selection
                .as_ref()
                .map(crate::selection::Selection::ordered_cells),
            Some(((0, 0), (0, 4)))
        );
        assert!(app
            .state
            .selection
            .as_ref()
            .is_some_and(crate::selection::Selection::is_finalized));
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
        assert!(app.selection_highlight_clear_deadline.is_none());
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn copy_on_select_disabled_keeps_explicit_double_click_copy() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        app.state.copy_on_select = false;
        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y;

        double_click(&mut app, col, row);

        assert_eq!(clipboard_write_content(&mut app), b"alpha");
        assert_visible_selection(&app);
        assert!(app.selection_highlight_clear_deadline.is_some());
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn new_drag_cancels_stale_double_click_highlight_deadline() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        app.state.copy_on_select = false;
        let row = info.inner_rect.y;
        let word_col = info.inner_rect.x + 2;

        double_click(&mut app, word_col, row);
        assert_eq!(clipboard_write_content(&mut app), b"alpha");
        let stale_deadline = app
            .selection_highlight_clear_deadline
            .expect("double-click highlight deadline");

        let start_col = info.inner_rect.x + 6;
        let end_col = info.inner_rect.x + 9;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_col,
            row,
        ));
        assert!(app.selection_highlight_clear_deadline.is_none());
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

        assert_visible_selection(&app);
        assert!(!app
            .clear_due_selection_highlight(stale_deadline + std::time::Duration::from_millis(1)));
        assert_visible_selection(&app);
        assert!(app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn ignored_left_down_keeps_double_click_highlight_deadline() {
        let (mut app, info) = app_with_screen_bytes(b"alpha beta");
        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y;

        double_click(&mut app, col, row);
        assert_eq!(clipboard_write_content(&mut app), b"alpha");
        let deadline = app
            .selection_highlight_clear_deadline
            .expect("double-click highlight deadline");
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "finished".into(),
            context: "missing".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: "missing".into(),
                pane_id: info.id,
            }),
        });
        app.state.view.toast_hit_area = Rect::new(0, 0, 1, 1);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));

        assert_visible_selection(&app);
        assert_eq!(app.selection_highlight_clear_deadline, Some(deadline));
        assert!(app.clear_due_selection_highlight(deadline + std::time::Duration::from_millis(1)));
        assert!(app.state.selection.is_none());
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

    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_click_url_invokes_plugin_link_handler_but_super_click_does_not() {
        let line = "see https://github.com/ogulcancelik/herdr/issues/398";
        let col = line.find("github").expect("url host") as u16;

        let (mut ctrl_app, ctrl_info) = app_with_screen_bytes(line.as_bytes());
        install_test_link_handler(&mut ctrl_app);
        ctrl_app.handle_mouse(modified_mouse(
            MouseEventKind::Down(MouseButton::Left),
            ctrl_info.inner_rect.x + col,
            ctrl_info.inner_rect.y,
            KeyModifiers::CONTROL,
        ));

        let ctrl_log = ctrl_app
            .state
            .plugin_command_logs
            .last()
            .expect("ctrl-click should start plugin link handler");
        assert_eq!(ctrl_log.plugin_id, "example.links");
        assert_eq!(ctrl_log.action_id.as_deref(), Some("open"));

        let (mut super_app, super_info) = app_with_screen_bytes(line.as_bytes());
        install_test_link_handler(&mut super_app);
        super_app.handle_mouse(modified_mouse(
            MouseEventKind::Down(MouseButton::Left),
            super_info.inner_rect.x + col,
            super_info.inner_rect.y,
            KeyModifiers::SUPER,
        ));

        assert!(super_app.state.plugin_command_logs.is_empty());
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
    async fn pane_cell_url_resolver_finds_soft_wrapped_url() {
        let (_app, info) = app_with_screen_bytes(b"");
        let prefix = "https://example.com/";
        let padding = "a".repeat(info.inner_rect.width as usize - prefix.len());
        let url = format!("{prefix}{padding}tail");
        let (app, _info) = app_with_screen_bytes(url.as_bytes());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 1, 1)
                .as_deref(),
            Some(url.as_str())
        );
    }

    #[tokio::test]
    async fn pane_cell_url_resolver_does_not_shift_after_zero_width_mark() {
        let url = "https://example.com/mark";
        let screen = format!("e\u{301} {url}");
        let (app, _info) = app_with_screen_bytes(screen.as_bytes());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 0, 2)
                .as_deref(),
            Some(url)
        );
    }

    #[tokio::test]
    async fn pane_cell_url_resolver_handles_hard_newline_after_full_row() {
        let (_app, info) = app_with_screen_bytes(b"");
        let full_row = "x".repeat(info.inner_rect.width as usize);
        let url = "https://example.com/next";
        let screen = format!("{full_row}\n{url}");
        let (app, _info) = app_with_screen_bytes(screen.as_bytes());
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 1, 1)
                .as_deref(),
            None
        );
        assert_eq!(
            app.state
                .url_at_pane_cell(&app.terminal_runtimes, pane_id, 2, 1)
                .as_deref(),
            Some(url)
        );
    }

    #[tokio::test]
    async fn render_stream_does_not_synthesize_soft_wrapped_url_hyperlinks() {
        let (_app, info) = app_with_screen_bytes(b"");
        let prefix = "https://example.com/";
        let padding = "b".repeat(info.inner_rect.width as usize - prefix.len());
        let url = format!("{prefix}{padding}tail");
        let (app, _info) = app_with_screen_bytes(url.as_bytes());

        let links =
            crate::server::render_stream::visible_hyperlinks(&app.state, &app.terminal_runtimes);

        assert!(links.is_empty());
    }

    #[tokio::test]
    async fn render_stream_does_not_synthesize_url_hyperlinks_after_zero_width_mark() {
        let url = "https://example.com/mark";
        let screen = format!("e\u{301} {url}");
        let (app, _info) = app_with_screen_bytes(screen.as_bytes());

        let links =
            crate::server::render_stream::visible_hyperlinks(&app.state, &app.terminal_runtimes);

        assert!(links.is_empty());
    }

    #[tokio::test]
    async fn render_stream_does_not_synthesize_hard_newline_plain_url_hyperlinks() {
        let (_app, info) = app_with_screen_bytes(b"");
        let full_row = "x".repeat(info.inner_rect.width as usize);
        let url = "https://example.com/next";
        let screen = format!("{full_row}\n{url}");
        let (app, _info) = app_with_screen_bytes(screen.as_bytes());
        let links =
            crate::server::render_stream::visible_hyperlinks(&app.state, &app.terminal_runtimes);

        assert!(links.is_empty());
    }

    #[tokio::test]
    async fn render_stream_exports_osc8_hyperlink_metadata() {
        let uri = "https://example.com/target";
        let (mut app, _info) =
            app_with_screen_bytes(format!("\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\").as_bytes());
        let (buffer, cursor) = crate::server::render_stream::render_virtual_with_runtime_registry(
            &mut app.state,
            &app.terminal_runtimes,
            ratatui::layout::Rect::new(0, 0, 106, 20),
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let links =
            crate::server::render_stream::visible_hyperlinks(&app.state, &app.terminal_runtimes);
        let frame = crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            &buffer, cursor, &links,
        );
        let ((x, y), symbol, _) = links
            .iter()
            .find(|(_, symbol, link_uri)| symbol == "l" && link_uri == uri)
            .expect("OSC 8 link cell");
        let linked_cell_index = usize::from(*y) * usize::from(frame.width) + usize::from(*x);

        assert_eq!(frame.hyperlinks, vec![uri.to_owned()]);
        assert_eq!(symbol, "l");
        assert_eq!(frame.cells[linked_cell_index].hyperlink, Some(0));
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
    async fn copy_on_select_disabled_still_forwards_mouse_reporting_gestures() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.copy_on_select = false;

        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            col + 1,
            row + 1,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            col + 1,
            row + 1,
        ));

        assert!(app.event_rx.try_recv().is_err());
        assert!(app.state.selection.is_none());
        assert!(app.selection_highlight_clear_deadline.is_none());
        assert_eq!(
            input_rx.try_recv().expect("forwarded left mouse down"),
            Bytes::from_static(b"\x1b[<0;3;4M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded left mouse drag"),
            Bytes::from_static(b"\x1b[<32;4;5M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded left mouse up"),
            Bytes::from_static(b"\x1b[<0;4;5m")
        );
        assert!(input_rx.try_recv().is_err());
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
            width: None,
            height: None,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_custom_pane_command_opens_overlay_pane() {
        let mut app = app_with_spawned_workspace();

        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::direct("ctrl+alt+g"),
            label: "ctrl+alt+g".into(),
            command: "printf direct-pane".into(),
            action: crate::config::CustomCommandAction::Pane,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.mode, Mode::Terminal);

        shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_custom_popup_command_opens_layout_neutral_popup() {
        let mut app = app_with_spawned_workspace();

        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::direct("ctrl+alt+g"),
            label: "ctrl+alt+g".into(),
            command: "sleep 1".into(),
            action: crate::config::CustomCommandAction::Popup,
            description: None,
            width: Some(crate::popup_size::PopupSize::Cells(60)),
            height: Some(crate::popup_size::PopupSize::Cells(12)),
        }];

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        assert!(app.state.popup_pane.is_some());
        assert!(!app
            .popup_runtime()
            .unwrap()
            .agent_detection_enabled_for_test());
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.mode, Mode::Terminal);
        let snapshot = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
            app.state.sidebar_width,
            app.state.sidebar_section_split,
            app.state.collapsed_space_keys.clone(),
        );
        assert_eq!(snapshot.workspaces[0].tabs[0].panes.len(), 1);
        assert!(matches!(
            snapshot.workspaces[0].tabs[0].layout,
            crate::persist::LayoutSnapshot::Pane(_)
        ));

        shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn direct_custom_popup_command_closes_after_exit() {
        let mut app = app_with_spawned_workspace();
        let focused_pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let focused_pane_id = app.public_pane_id(0, focused_pane).unwrap();

        let output_path = unique_temp_path("custom-popup-command");
        let command = format!(
            "printf '%s|%s' \"${{HERDR_PANE_ID-unset}}\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'",
            output_path.display()
        );
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::direct("ctrl+alt+g"),
            label: "ctrl+alt+g".into(),
            command,
            action: crate::config::CustomCommandAction::Popup,
            description: None,
            width: None,
            height: None,
        }];

        app.handle_terminal_key(TerminalKey::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .await;

        assert_eq!(
            wait_for_file(&output_path),
            format!("unset|{focused_pane_id}")
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.drain_internal_events();
            if app.state.popup_pane.is_none() {
                break;
            }
        }

        assert!(app.state.popup_pane.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);

        shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn popup_forwards_escape_instead_of_closing() {
        let mut app = app_for_mouse_test();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                40,
                2,
                1024,
                b"one\r\ntwo\r\nthree\r\n",
                4,
            );
        runtime.scroll_up(1);
        assert!(runtime
            .scroll_metrics()
            .is_some_and(|metrics| metrics.offset_from_bottom > 0));
        app.install_test_popup_runtime(runtime);
        app.state.mode = Mode::Settings;

        app.handle_terminal_key_headless(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(rx.try_recv().unwrap().as_ref(), b"\x1b");
        assert!(app.state.popup_pane.is_some());
        assert_eq!(
            app.popup_runtime()
                .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
                .map(|metrics| metrics.offset_from_bottom),
            Some(0)
        );
    }

    #[tokio::test]
    async fn local_popup_input_waits_for_channel_capacity() {
        let mut app = app_for_mouse_test();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(40, 2, 1);
        runtime
            .try_send_bytes(Bytes::from_static(b"queued"))
            .unwrap();
        app.install_test_popup_runtime(runtime);
        app.state.mode = Mode::Settings;

        let mut send = Box::pin(
            app.handle_terminal_key(TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty())),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut send)
                .await
                .is_err()
        );

        assert_eq!(rx.recv().await.unwrap().as_ref(), b"queued");
        send.await;
        assert_eq!(rx.recv().await.unwrap().as_ref(), b"x");
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

    #[tokio::test]
    async fn page_up_forwarded_to_primary_screen_application_cursor_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let mut bytes = b"\x1b[?1h".to_vec();
        bytes.extend_from_slice(&numbered_lines_bytes(64));
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &bytes,
                4,
            );
        ws.tabs[0].runtimes.insert(pane_id, runtime);

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

        let forwarded = input_rx.try_recv().expect("forwarded PageUp");
        assert_eq!(forwarded.as_ref(), b"\x1b[5~");
        let end_metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after PageUp");
        assert_eq!(end_metrics.offset_from_bottom, 0);
    }
}
