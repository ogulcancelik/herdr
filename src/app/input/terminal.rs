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
