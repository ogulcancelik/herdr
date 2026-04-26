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
    settings::open_settings,
};
use self::{
    modal::{
        modal_action_from_key, ModalAction, ONBOARDING_WELCOME_ACTIONS, RELEASE_NOTES_ACTIONS,
    },
    settings::SettingsAction,
};
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
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => self.open_settings_from_onboarding(),
            _ => match modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS) {
                Some(ModalAction::Continue) => self.open_settings_from_onboarding(),
                _ => {}
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
                self.state.sidebar_width_source =
                    crate::app::state::SidebarWidthSource::ConfigDefault;
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
                SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
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
fn state_with_workspaces(names: &[&str]) -> AppState {
    let mut state = AppState::test_new();
    state.workspaces = names
        .iter()
        .map(|name| crate::workspace::Workspace::test_new(name))
        .collect();
    if !state.workspaces.is_empty() {
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
    }
    state
}

#[cfg(test)]
fn app_for_mouse_test() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        true,
        None,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.mode = Mode::Terminal;
    app.state.update_available = None;
    app.state.latest_release_notes_available = false;
    app.state.view.sidebar_rect = ratatui::layout::Rect::new(0, 0, 26, 20);
    app.state.view.terminal_area = ratatui::layout::Rect::new(26, 0, 80, 20);
    app
}

#[cfg(test)]
fn mouse(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::empty(),
    }
}

#[cfg(test)]
fn numbered_lines_bytes(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("{i:06}\r\n"))
        .collect::<String>()
        .into_bytes()
}

#[cfg(test)]
fn capture_snapshot(state: &AppState) -> crate::persist::SessionSnapshot {
    crate::persist::capture(
        &state.workspaces,
        state.active,
        state.selected,
        state.agent_panel_scope,
        state.sidebar_width,
        state.sidebar_section_split,
    )
}

#[cfg(test)]
fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
}

#[cfg(test)]
fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
fn wait_for_file(path: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(path) {
            return content;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}
