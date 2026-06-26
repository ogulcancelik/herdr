use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{
    command_palette::{
        build_keybind_help_model, can_execute_keybind_command, keybind_help_action_context,
        keybind_help_selected_row, normalize_keybind_help_selection, KeybindCommand,
        KeybindHelpModel, KeybindHelpRow,
    },
    state::{AppState, ToastKind, ToastNotification},
    App, Mode,
};

impl App {
    fn execute_command_with_feedback(
        &mut self,
        command: KeybindCommand,
        enabled: bool,
        reason: Option<String>,
        context: crate::app::input::ActionContext,
    ) {
        if !enabled {
            let previous_toast = self.state.toast.clone();
            self.state.toast = Some(ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "command unavailable".to_string(),
                context: reason.unwrap_or_else(|| "not available in this context".to_string()),
                position: None,
                target: None,
            });
            self.sync_toast_deadline(previous_toast);
            return;
        }

        match command {
            KeybindCommand::Navigate(action) => {
                super::navigate::execute_navigate_action_in_context(
                    &mut self.state,
                    &mut self.terminal_runtimes,
                    action,
                    context,
                );
            }
            KeybindCommand::CustomCommand(index) => {
                if let Some(binding) = self.state.keybinds.custom_commands.get(index).cloned() {
                    self.launch_custom_command(binding, context);
                }
            }
        }
    }

    pub(crate) fn handle_keybind_help_key(&mut self, key: KeyEvent) {
        let model = build_keybind_help_model(&self.state);
        normalize_keybind_help_selection(&mut self.state, &model);
        let page = (self
            .state
            .keybind_help_body_rect()
            .unwrap_or_default()
            .height
            / 2)
        .max(1) as usize;

        if self.state.keybind_help.search_focused {
            match key.code {
                KeyCode::Esc => {
                    self.state.keybind_help.search_focused = false;
                    self.state.keybind_help.query.clear();
                }
                KeyCode::Backspace => {
                    self.state.keybind_help.query.pop();
                }
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                    self.state.keybind_help.query.clear();
                }
                KeyCode::Char(ch)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.state.keybind_help.query.push(ch);
                }
                _ => self.handle_keybind_help_navigation_or_action(key, &model, page),
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => super::modal::leave_modal(&mut self.state),
                KeyCode::Char('/') => self.state.keybind_help.search_focused = true,
                _ => self.handle_keybind_help_navigation_or_action(key, &model, page),
            }
        }

        normalize_keybind_help_selection_with_scroll(&mut self.state);
    }

    fn handle_keybind_help_navigation_or_action(
        &mut self,
        key: KeyEvent,
        model: &KeybindHelpModel,
        page: usize,
    ) {
        let max_idx = model.entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.keybind_help.selected =
                    self.state.keybind_help.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.keybind_help.selected = self
                    .state
                    .keybind_help
                    .selected
                    .saturating_add(1)
                    .min(max_idx);
            }
            KeyCode::PageUp => {
                self.state.keybind_help.selected =
                    self.state.keybind_help.selected.saturating_sub(page);
            }
            KeyCode::PageDown => {
                self.state.keybind_help.selected = self
                    .state
                    .keybind_help
                    .selected
                    .saturating_add(page)
                    .min(max_idx);
            }
            KeyCode::Home => {
                self.state.keybind_help.selected = 0;
            }
            KeyCode::End => {
                self.state.keybind_help.selected = max_idx;
            }
            KeyCode::Enter => {
                if let Some(entry) = model.entries.get(self.state.keybind_help.selected).cloned() {
                    if self.execute_keybind_help_command(entry.command)
                        && self.state.mode == Mode::KeybindHelp
                    {
                        super::modal::leave_modal(&mut self.state);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn keybind_help_hover_row(&mut self, row: usize) {
        let model = build_keybind_help_model(&self.state);
        let Some(visible_row) = model
            .rows
            .get(self.state.keybind_help.scroll as usize + row)
        else {
            return;
        };
        if let KeybindHelpRow::Entry(idx) = visible_row {
            self.state.keybind_help.selected = *idx;
        }
    }

    pub(super) fn keybind_help_click_row(&mut self, row: usize) {
        let model = build_keybind_help_model(&self.state);
        let Some(visible_row) = model
            .rows
            .get(self.state.keybind_help.scroll as usize + row)
        else {
            return;
        };
        if let KeybindHelpRow::Entry(idx) = visible_row {
            self.state.keybind_help.selected = *idx;
            if let Some(entry) = model.entries.get(*idx).cloned() {
                if self.execute_keybind_help_command(entry.command)
                    && self.state.mode == Mode::KeybindHelp
                {
                    super::modal::leave_modal(&mut self.state);
                }
            }
        }
    }

    fn execute_keybind_help_command(&mut self, command: KeybindCommand) -> bool {
        let context = keybind_help_action_context(&self.state);
        let (enabled, reason) = can_execute_keybind_command(
            &self.state,
            command,
            context,
            Some(&self.terminal_runtimes),
        );
        self.execute_command_with_feedback(command, enabled, reason, context);
        enabled
    }
}

pub(crate) fn insert_keybind_help_search_text(state: &mut AppState, text: &str) {
    if state.mode != Mode::KeybindHelp || !state.keybind_help.search_focused {
        return;
    }
    state.keybind_help.query.push_str(text);
    normalize_keybind_help_selection_with_scroll(state);
}

fn normalize_keybind_help_selection_with_scroll(state: &mut AppState) {
    let model = build_keybind_help_model(state);
    normalize_keybind_help_selection(state, &model);
    state.scroll_keybind_help(0);
    if let Some(selected_row) = keybind_help_selected_row(&model, state.keybind_help.selected) {
        let viewport = state.keybind_help_body_rect().unwrap_or_default().height as usize;
        if selected_row < state.keybind_help.scroll as usize {
            state.keybind_help.scroll = selected_row as u16;
        } else {
            let visible_end = state.keybind_help.scroll.saturating_add(viewport as u16) as usize;
            if selected_row >= visible_end {
                state.keybind_help.scroll = selected_row
                    .saturating_add(1)
                    .saturating_sub(viewport)
                    .min(u16::MAX as usize) as u16;
            }
        }
    }
}
