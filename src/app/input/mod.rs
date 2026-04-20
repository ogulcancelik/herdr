//! Input handling — translates crossterm key/mouse events into state mutations.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::input::TerminalKey;
use ratatui::layout::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

const WORKSPACE_DRAG_THRESHOLD: u16 = 1;
const TAB_DRAG_THRESHOLD: u16 = 1;

mod modal;
mod mouse;
mod navigate;
mod overlays;
mod selection;
mod settings;
mod sidebar;
mod terminal;

pub(crate) use self::{
    modal::{
        handle_confirm_close_key, handle_context_menu_key, handle_global_menu_key,
        handle_keybind_help_key, handle_rename_key, handle_resize_key,
    },
    navigate::{handle_navigate_key, terminal_direct_navigation_action},
};
#[cfg(test)]
use self::{
    modal::{modal_action_from_buttons, open_new_tab_dialog, open_rename_active_tab},
    mouse::wheel_routing,
    navigate::{execute_navigate_action, handle_navigate_reserved_key, NavigateAction},
    settings::{open_settings, update_settings_state},
};
use self::{
    modal::{
        modal_action_from_key, ModalAction, ONBOARDING_NOTIFICATION_ACTIONS,
        ONBOARDING_WELCOME_ACTIONS, RELEASE_NOTES_ACTIONS,
    },
    settings::SettingsAction,
};
#[cfg(test)]
use super::state::{AgentPanelScope, ContextMenuKind, ContextMenuState, DragTarget, MenuListState};
use super::state::{AppState, Mode};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

impl App {
    pub(super) async fn handle_key(&mut self, key: TerminalKey) {
        match self.state.mode {
            Mode::Terminal => self.handle_terminal_key(key).await,
            _ => {
                let key = key.as_key_event();
                match self.state.mode {
                    Mode::Onboarding => self.handle_onboarding_key(key),
                    Mode::ReleaseNotes => self.handle_release_notes_key(key),
                    Mode::Navigate => self.handle_navigate_key(key),
                    Mode::RenameWorkspace | Mode::RenameTab => {
                        handle_rename_key(&mut self.state, key)
                    }
                    Mode::Resize => handle_resize_key(&mut self.state, key),
                    Mode::ConfirmClose => handle_confirm_close_key(&mut self.state, key),
                    Mode::ContextMenu => handle_context_menu_key(&mut self.state, key),
                    Mode::Settings => self.handle_settings_key(key),
                    Mode::GlobalMenu => handle_global_menu_key(&mut self.state, key),
                    Mode::KeybindHelp => handle_keybind_help_key(&mut self.state, key),
                    Mode::Terminal => unreachable!(),
                }
            }
        }
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.mode != Mode::Terminal {
            return;
        }
        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                let _ = rt.send_paste(text).await;
            }
        }
    }

    pub(crate) fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match self.state.onboarding_step {
            0 => match key.code {
                KeyCode::Right | KeyCode::Char('l') => {
                    self.state.onboarding_step = 1;
                }
                _ => match modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS) {
                    Some(ModalAction::Continue) => self.state.onboarding_step = 1,
                    _ => {}
                },
            },
            _ => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.state.onboarding_list.move_prev(),
                KeyCode::Down | KeyCode::Char('j') => self.state.onboarding_list.move_next(4),
                KeyCode::Left | KeyCode::Char('h') => {
                    self.state.onboarding_step = 0;
                }
                KeyCode::Char(c) if ('1'..='4').contains(&c) => {
                    self.state
                        .onboarding_list
                        .select((c as usize) - ('1' as usize));
                }
                _ => match modal_action_from_key(&key, ONBOARDING_NOTIFICATION_ACTIONS) {
                    Some(ModalAction::Back) => self.state.onboarding_step = 0,
                    Some(ModalAction::Save) => self.complete_onboarding(),
                    _ => {}
                },
            },
        }
    }

    pub(crate) fn handle_release_notes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_release_notes(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_release_notes(1),
            KeyCode::PageUp => self.scroll_release_notes(-8),
            KeyCode::PageDown => self.scroll_release_notes(8),
            KeyCode::Home => {
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.release_notes_max_scroll();
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = max_scroll;
                }
            }
            _ => match modal_action_from_key(&key, RELEASE_NOTES_ACTIONS) {
                Some(ModalAction::Close) => self.dismiss_release_notes(),
                _ => {}
            },
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.handle_overlay_mouse(mouse) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width = self.state.default_sidebar_width;
                self.state.sidebar_width_auto = false;
                self.state.mark_session_dirty();
                self.state.drag = None;
                return;
            }
        }

        if let Some(action) = self.state.handle_mouse(mouse) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToast(enabled) => self.save_toast(enabled),
            }
        }

        if let Some(content) = self.state.request_clipboard_write.take() {
            if self
                .event_tx
                .try_send(crate::events::AppEvent::ClipboardWrite { content })
                .is_err()
            {
                tracing::warn!("failed to queue clipboard write event");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

// Note: split_pane needs runtime (event_tx for PTY spawn), so it lives on App
impl AppState {
    pub(crate) fn split_pane(&mut self, direction: Direction) {
        // Actual PTY spawning happens in Workspace::split_focused
        // which needs events channel — this is called from navigate_key
        // where we don't have async context, so the workspace handles it
        let (rows, cols) = self.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);

        let cwd = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| ws.focused_runtime())
            .and_then(|rt| rt.cwd());

        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if let Ok(new_id) = ws.split_focused(
                direction,
                new_rows,
                new_cols,
                cwd,
                self.pane_scrollback_limit_bytes,
                self.host_terminal_theme,
            ) {
                ws.layout.focus_pane(new_id);
                self.mark_session_dirty();
                self.mode = Mode::Terminal;
            }
        }
    }
}

#[cfg(test)]
mod tests;
