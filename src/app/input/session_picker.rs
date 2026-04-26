use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::state::AppState;
use crate::persist::{SessionId, SessionName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionPickerAction {
    Switch(SessionId),
    Create(SessionName),
    Delete(SessionName),
}

pub(crate) fn update_session_picker_state(
    state: &mut AppState,
    key: KeyEvent,
) -> Option<SessionPickerAction> {
    let picker = &mut state.session_picker;

    if picker.creating {
        match key.code {
            KeyCode::Esc => {
                picker.creating = false;
                picker.name_input.clear();
                picker.error = None;
            }
            KeyCode::Enter => {
                let name = picker.name_input.trim();
                match crate::persist::validate_session_name(name) {
                    Ok(name) => {
                        picker.creating = false;
                        picker.name_input.clear();
                        picker.error = None;
                        return Some(SessionPickerAction::Create(name));
                    }
                    Err(err) => {
                        picker.error = Some(err);
                    }
                }
            }
            KeyCode::Backspace => {
                picker.name_input.pop();
                picker.error = None;
            }
            KeyCode::Char(c) => {
                if picker.name_input.len() < 32 {
                    picker.name_input.push(c);
                }
                picker.error = None;
            }
            _ => {}
        }
        return None;
    }

    if let Some(pending_name) = picker.pending_delete.clone() {
        match key.code {
            KeyCode::Esc => {
                picker.pending_delete = None;
            }
            KeyCode::Enter => {
                picker.pending_delete = None;
                return Some(SessionPickerAction::Delete(pending_name));
            }
            _ => {}
        }
        return None;
    }

    match key.code {
        KeyCode::Esc => {
            state.mode = if state.active.is_some() {
                crate::app::Mode::Terminal
            } else {
                crate::app::Mode::Navigate
            };
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if picker.highlighted > 0 {
                picker.highlighted -= 1;
            }
            ensure_highlight_visible(picker);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !picker.sessions.is_empty() && picker.highlighted < picker.sessions.len() - 1 {
                picker.highlighted += 1;
            }
            ensure_highlight_visible(picker);
        }
        KeyCode::Enter => {
            if let Some(entry) = picker.sessions.get(picker.highlighted) {
                if !entry.active {
                    return Some(SessionPickerAction::Switch(entry.id.clone()));
                }
            }
        }
        KeyCode::Char('n') => {
            picker.creating = true;
            picker.name_input.clear();
            picker.error = None;
        }
        KeyCode::Char('d') => {
            if let Some(entry) = picker.sessions.get(picker.highlighted) {
                if entry.deletable {
                    if let SessionId::Named(name) = &entry.id {
                        picker.pending_delete = Some(name.clone());
                    }
                }
            }
        }
        _ => {}
    }

    None
}

fn ensure_highlight_visible(picker: &mut crate::app::state::SessionPickerState) {
    const MAX_VISIBLE_ROWS: usize = 6;
    if picker.highlighted < picker.scroll {
        picker.scroll = picker.highlighted;
    } else if picker.highlighted >= picker.scroll + MAX_VISIBLE_ROWS {
        picker.scroll = picker.highlighted.saturating_sub(MAX_VISIBLE_ROWS - 1);
    }
}

impl AppState {
    pub(super) fn handle_session_picker_mouse(
        &mut self,
        mouse: MouseEvent,
    ) -> Option<SessionPickerAction> {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return None;
        }

        let screen = self.screen_rect();
        let modal = crate::ui::session_picker_modal_rect(screen)?;
        let inner = Rect::new(modal.x + 1, modal.y + 1, modal.width - 2, modal.height - 2);

        // Click outside modal closes it
        if mouse.column < modal.x
            || mouse.column >= modal.x + modal.width
            || mouse.row < modal.y
            || mouse.row >= modal.y + modal.height
        {
            self.mode = if self.active.is_some() {
                crate::app::Mode::Terminal
            } else {
                crate::app::Mode::Navigate
            };
            return None;
        }

        let header_h = 2u16;
        let footer_h = 1u16;
        let content = Rect::new(
            inner.x,
            inner.y + header_h,
            inner.width,
            inner.height.saturating_sub(header_h + footer_h),
        );

        let visible_rows = crate::ui::session_picker_visible_row_rects(
            content,
            self.session_picker.sessions.len(),
            self.session_picker.scroll,
        );

        for (idx, row) in visible_rows {
            if mouse.row >= row.y
                && mouse.row < row.y + row.height
                && mouse.column >= row.x
                && mouse.column < row.x + row.width
            {
                self.session_picker.highlighted = idx;
                let entry = &self.session_picker.sessions[idx];
                if !entry.active {
                    return Some(SessionPickerAction::Switch(entry.id.clone()));
                }
                return None;
            }
        }

        // Check new button in footer
        let footer = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(footer_h),
            inner.width,
            footer_h,
        );
        let new_btn = crate::ui::session_picker_new_button_rect(footer);
        if mouse.column >= new_btn.x
            && mouse.column < new_btn.x + new_btn.width
            && mouse.row >= new_btn.y
            && mouse.row < new_btn.y + new_btn.height
        {
            self.session_picker.creating = true;
            self.session_picker.name_input.clear();
            self.session_picker.error = None;
        }

        None
    }
}
