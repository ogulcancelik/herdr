use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::{
        state::{CopyModeSearchDirection, CopyModeSearchPrompt, CopyModeSelection, CopyModeState},
        App, AppState, Mode,
    },
    input::TerminalKey,
    selection::Selection,
    terminal::TerminalRuntimeRegistry,
};

impl App {
    pub(crate) fn handle_copy_mode_key(&mut self, key: TerminalKey) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.state.update_dismissed = true;
        if self.state.is_prefix_key(key) {
            self.state.mode = Mode::Prefix;
            return;
        }
        self.state
            .handle_copy_mode_key(&self.terminal_runtimes, key);
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

impl AppState {
    pub(crate) fn enter_copy_mode(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(pane_id) = self
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.focused_pane_id())
        else {
            return;
        };
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        if info.inner_rect.width == 0 || info.inner_rect.height == 0 {
            return;
        }

        let cursor = self
            .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            .and_then(|rt| rt.cursor_state(info.inner_rect, true))
            .filter(|cursor| cursor.visible)
            .map(|cursor| {
                (
                    cursor.y.saturating_sub(info.inner_rect.y),
                    cursor.x.saturating_sub(info.inner_rect.x),
                )
            })
            .unwrap_or_else(|| (info.inner_rect.height.saturating_sub(1), 0));
        let entry_offset_from_bottom = self
            .pane_scroll_metrics(terminal_runtimes, pane_id)
            .map_or(0, |metrics| metrics.offset_from_bottom);

        self.clear_selection();
        self.copy_mode = Some(CopyModeState {
            pane_id,
            cursor_row: cursor.0.min(info.inner_rect.height.saturating_sub(1)),
            cursor_col: cursor.1.min(info.inner_rect.width.saturating_sub(1)),
            entry_offset_from_bottom,
            selection: None,
            search: crate::app::state::CopyModeSearchState {
                geometry: Some((info.inner_rect.width, info.inner_rect.height)),
                ..Default::default()
            },
        });
        self.mode = Mode::Copy;
    }

    pub(crate) fn handle_copy_mode_key(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        key: TerminalKey,
    ) {
        if self.handle_copy_mode_search_prompt_key(terminal_runtimes, key) {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                let should_clear = self.copy_mode.as_ref().is_some_and(|copy_mode| {
                    copy_mode.selection.is_some()
                        || !copy_mode.search.query.is_empty()
                        || !copy_mode.search.matches.is_empty()
                        || copy_mode.search.direction.is_some()
                });
                if should_clear {
                    self.clear_copy_mode_selection();
                    if let Some(search) = self
                        .copy_mode
                        .as_mut()
                        .map(|copy_mode| &mut copy_mode.search)
                    {
                        let geometry = search.geometry;
                        *search = crate::app::state::CopyModeSearchState {
                            geometry,
                            ..Default::default()
                        };
                    }
                    return;
                }
                self.exit_copy_mode(terminal_runtimes, false);
                return;
            }
            KeyCode::Enter => {
                self.exit_copy_mode(terminal_runtimes, true);
                return;
            }
            KeyCode::Left => {
                self.move_copy_cursor(terminal_runtimes, 0, -1);
                return;
            }
            KeyCode::Down => {
                self.move_copy_cursor(terminal_runtimes, 1, 0);
                return;
            }
            KeyCode::Up => {
                self.move_copy_cursor(terminal_runtimes, -1, 0);
                return;
            }
            KeyCode::Right => {
                self.move_copy_cursor(terminal_runtimes, 0, 1);
                return;
            }
            KeyCode::PageUp => {
                self.scroll_copy_mode_page(terminal_runtimes, -1, false);
                return;
            }
            KeyCode::PageDown => {
                self.scroll_copy_mode_page(terminal_runtimes, 1, false);
                return;
            }
            KeyCode::Home => {
                self.copy_mode_line_edge(terminal_runtimes, false);
                return;
            }
            KeyCode::End => {
                self.copy_mode_line_edge(terminal_runtimes, true);
                return;
            }
            _ => {}
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('b'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_copy_mode_page(terminal_runtimes, -1, false)
            }
            (KeyCode::Char('f'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_copy_mode_page(terminal_runtimes, 1, false)
            }
            (KeyCode::Char('u'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_copy_mode_page(terminal_runtimes, -1, true)
            }
            (KeyCode::Char('d'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.scroll_copy_mode_page(terminal_runtimes, 1, true)
            }
            _ => {}
        }

        let Some(ch) = copy_mode_command_char(key) else {
            return;
        };
        match ch {
            'q' => self.exit_copy_mode(terminal_runtimes, false),
            'y' => self.exit_copy_mode(terminal_runtimes, true),
            'v' | ' ' => self.begin_copy_mode_selection(terminal_runtimes),
            'V' => self.select_copy_mode_line(terminal_runtimes),
            'h' => self.move_copy_cursor(terminal_runtimes, 0, -1),
            'j' => self.move_copy_cursor(terminal_runtimes, 1, 0),
            'k' => self.move_copy_cursor(terminal_runtimes, -1, 0),
            'l' => self.move_copy_cursor(terminal_runtimes, 0, 1),
            'g' => self.copy_mode_history_top(terminal_runtimes),
            'G' => self.copy_mode_history_bottom(terminal_runtimes),
            '0' => self.copy_mode_line_edge(terminal_runtimes, false),
            '$' => self.copy_mode_line_edge(terminal_runtimes, true),
            '^' => self.copy_mode_first_non_blank(terminal_runtimes),
            '/' => self.open_copy_mode_search(CopyModeSearchDirection::Forward),
            '?' => self.open_copy_mode_search(CopyModeSearchDirection::Backward),
            'n' => self.repeat_copy_mode_search(terminal_runtimes, false),
            'N' => self.repeat_copy_mode_search(terminal_runtimes, true),
            'w' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::NextStart),
            'b' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::PreviousStart),
            'e' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::NextEnd),
            '{' => self.copy_mode_paragraph(terminal_runtimes, -1),
            '}' => self.copy_mode_paragraph(terminal_runtimes, 1),
            _ => {}
        }
    }

    fn handle_copy_mode_search_prompt_key(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        key: TerminalKey,
    ) -> bool {
        let Some(copy_mode) = self.copy_mode.as_mut() else {
            return false;
        };
        let Some(prompt) = copy_mode.search.prompt.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc => {
                copy_mode.search.prompt = None;
            }
            KeyCode::Enter => {
                let direction = prompt.direction;
                let query = std::mem::take(&mut prompt.query);
                copy_mode.search.prompt = None;
                self.submit_copy_mode_search(terminal_runtimes, query, direction, false);
            }
            KeyCode::Backspace => {
                prompt.query.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.query.clear();
            }
            _ => {
                if let Some(ch) = copy_mode_command_char(key) {
                    prompt.query.push(ch);
                }
            }
        }
        true
    }

    fn open_copy_mode_search(&mut self, direction: CopyModeSearchDirection) {
        let Some(copy_mode) = self.copy_mode.as_mut() else {
            return;
        };
        copy_mode.search.prompt = Some(CopyModeSearchPrompt {
            direction,
            query: String::new(),
        });
    }

    fn repeat_copy_mode_search(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        reverse: bool,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        if copy_mode.search.query.is_empty() {
            return;
        }
        let Some(mut direction) = copy_mode.search.direction else {
            return;
        };
        if reverse {
            direction = direction.reversed();
        }
        self.submit_copy_mode_search(
            terminal_runtimes,
            copy_mode.search.query.clone(),
            direction,
            true,
        );
    }

    fn submit_copy_mode_search(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        query: String,
        direction: CopyModeSearchDirection,
        repeat: bool,
    ) {
        if query.is_empty() {
            return;
        }
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(runtime) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
        else {
            return;
        };
        let Some(metrics) = runtime.scroll_metrics() else {
            return;
        };
        let cursor = crate::pane::TerminalTextPoint {
            row: viewport_top_row(metrics).saturating_add(u32::from(copy_mode.cursor_row)),
            col: copy_mode.cursor_col,
        };
        let previous_match = repeat
            .then(|| {
                copy_mode
                    .search
                    .current
                    .and_then(|index| copy_mode.search.matches.get(index).copied())
            })
            .flatten()
            .filter(|text_match| {
                text_match.start == cursor && runtime.text_match_is_current(*text_match)
            });
        let matches = runtime.search_text_matches(&query, query.chars().any(char::is_uppercase));
        let current = search_match_index(&matches, direction, cursor, previous_match);

        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.search.query = query;
            if !repeat {
                copy_mode.search.direction = Some(direction);
            }
            copy_mode.search.matches = matches;
            copy_mode.search.current = current;
        }

        let Some(target) = current.and_then(|index| {
            self.copy_mode
                .as_ref()
                .and_then(|copy_mode| copy_mode.search.matches.get(index).copied())
        }) else {
            return;
        };
        self.move_copy_cursor_to_absolute(terminal_runtimes, target.start, true);
    }

    fn move_copy_cursor_to_absolute(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        target: crate::pane::TerminalTextPoint,
        reserve_overlay_row: bool,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        let Some(metrics) = self.pane_scroll_metrics(terminal_runtimes, pane_id) else {
            return;
        };
        let current_top = viewport_top_row(metrics);
        let max_cursor_row = info
            .inner_rect
            .height
            .saturating_sub(if reserve_overlay_row { 2 } else { 1 });
        let desired_top = if target.row < current_top {
            target.row
        } else if target.row > current_top.saturating_add(u32::from(max_cursor_row)) {
            target.row.saturating_sub(u32::from(max_cursor_row))
        } else {
            current_top
        };
        let desired_offset = metrics
            .max_offset_from_bottom
            .saturating_sub(desired_top as usize);
        self.set_pane_scroll_offset(terminal_runtimes, pane_id, desired_offset);
        let Some(updated_metrics) = self.pane_scroll_metrics(terminal_runtimes, pane_id) else {
            return;
        };
        let updated_top = viewport_top_row(updated_metrics);
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_row = target
                .row
                .saturating_sub(updated_top)
                .min(u32::from(info.inner_rect.height.saturating_sub(1)))
                as u16;
            copy_mode.cursor_col = target.col.min(info.inner_rect.width.saturating_sub(1));
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    pub(crate) fn cancel_copy_mode(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        self.exit_copy_mode(terminal_runtimes, false);
    }

    fn exit_copy_mode(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, copy: bool) {
        let restore_scroll = self
            .copy_mode
            .as_ref()
            .map(|copy_mode| (copy_mode.pane_id, copy_mode.entry_offset_from_bottom));
        if copy {
            self.copy_selection(terminal_runtimes);
        } else {
            self.clear_selection();
        }
        if let Some((pane_id, offset_from_bottom)) = restore_scroll {
            self.set_pane_scroll_offset(terminal_runtimes, pane_id, offset_from_bottom);
        }
        self.copy_mode = None;
        self.mode = if self.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    fn begin_copy_mode_selection(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id).cloned() else {
            return;
        };
        if copy_mode.cursor_row >= info.inner_rect.height
            || copy_mode.cursor_col >= info.inner_rect.width
        {
            return;
        }

        let metrics = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id);
        self.selection = Some(Selection::anchor(
            copy_mode.pane_id,
            copy_mode.cursor_row,
            copy_mode.cursor_col,
            metrics,
        ));
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.selection = Some(CopyModeSelection::Character);
        }
    }

    fn select_copy_mode_line(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let cursor_row = copy_mode.cursor_row;
        let Some(info) = self.pane_info_by_id(pane_id) else {
            return;
        };
        let end_col = info.inner_rect.width.saturating_sub(1);
        let metrics = self.pane_scroll_metrics(terminal_runtimes, pane_id);
        let anchor_row = Selection::absolute_row_for_viewport(cursor_row, metrics);
        self.selection = Some(Selection::line_range(
            pane_id, anchor_row, anchor_row, end_col,
        ));
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.selection = Some(CopyModeSelection::Linewise { anchor_row });
        }
    }

    fn move_copy_cursor(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        row_delta: i16,
        col_delta: i16,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let mut cursor_row = copy_mode.cursor_row;
        let mut cursor_col = copy_mode.cursor_col;
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };

        if col_delta < 0 {
            cursor_col = cursor_col.saturating_sub(col_delta.unsigned_abs());
        } else if col_delta > 0 {
            cursor_col = cursor_col
                .saturating_add(col_delta as u16)
                .min(info.inner_rect.width.saturating_sub(1));
        }

        if row_delta < 0 {
            let delta = row_delta.unsigned_abs();
            if cursor_row >= delta {
                cursor_row -= delta;
            } else {
                self.scroll_pane_up(terminal_runtimes, pane_id, usize::from(delta));
                cursor_row = 0;
            }
        } else if row_delta > 0 {
            let delta = row_delta as u16;
            let bottom = info.inner_rect.height.saturating_sub(1);
            if cursor_row.saturating_add(delta) <= bottom {
                cursor_row += delta;
            } else {
                self.scroll_pane_down(terminal_runtimes, pane_id, usize::from(delta));
                cursor_row = bottom;
            }
        }

        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_row = cursor_row;
            copy_mode.cursor_col = cursor_col;
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn scroll_copy_mode_page(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        direction: i16,
        half_page: bool,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let mut cursor_row = copy_mode.cursor_row;
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let lines = copy_mode_page_lines(info.inner_rect.height, half_page);
        if let Some(metrics) = self.pane_scroll_metrics(terminal_runtimes, pane_id) {
            if direction < 0 {
                let next_offset = metrics.offset_from_bottom.saturating_add(lines);
                if next_offset > metrics.max_offset_from_bottom {
                    let scrolled_lines = metrics
                        .max_offset_from_bottom
                        .saturating_sub(metrics.offset_from_bottom);
                    let cursor_lines = lines.saturating_sub(scrolled_lines);
                    self.set_pane_scroll_offset(
                        terminal_runtimes,
                        pane_id,
                        metrics.max_offset_from_bottom,
                    );
                    cursor_row =
                        cursor_row.saturating_sub(cursor_lines.min(u16::MAX as usize) as u16);
                } else {
                    self.set_pane_scroll_offset(terminal_runtimes, pane_id, next_offset);
                }
            } else if metrics.offset_from_bottom < lines {
                let cursor_lines = lines.saturating_sub(metrics.offset_from_bottom);
                self.set_pane_scroll_offset(terminal_runtimes, pane_id, 0);
                cursor_row = cursor_row
                    .saturating_add(cursor_lines.min(u16::MAX as usize) as u16)
                    .min(info.inner_rect.height.saturating_sub(1));
            } else {
                self.set_pane_scroll_offset(
                    terminal_runtimes,
                    pane_id,
                    metrics.offset_from_bottom - lines,
                );
            }
        } else if direction < 0 {
            self.scroll_pane_up(terminal_runtimes, pane_id, lines);
        } else {
            self.scroll_pane_down(terminal_runtimes, pane_id, lines);
        }
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_row = cursor_row;
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_history_top(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let Some(metrics) = self.pane_scroll_metrics(terminal_runtimes, pane_id) else {
            return;
        };
        self.set_pane_scroll_offset(terminal_runtimes, pane_id, metrics.max_offset_from_bottom);
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_history_bottom(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let Some(info) = self.pane_info_by_id(pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let cursor_row = info.inner_rect.height.saturating_sub(1);
        self.set_pane_scroll_offset(terminal_runtimes, pane_id, 0);
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_row = cursor_row;
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_line_edge(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, end: bool) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let cursor_row = copy_mode.cursor_row;
        let Some(info) = self.pane_info_by_id(pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let cursor_col = if end {
            let Some(text) = self.copy_mode_visible_row_text(terminal_runtimes, cursor_row) else {
                return;
            };
            last_character_col(&text)
                .unwrap_or(0)
                .min(info.inner_rect.width.saturating_sub(1))
        } else {
            0
        };
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_col = cursor_col;
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_first_non_blank(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let cursor_row = copy_mode.cursor_row;
        let Some(text) = self.copy_mode_visible_row_text(terminal_runtimes, cursor_row) else {
            return;
        };
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.cursor_col = first_non_blank_col(&text).unwrap_or(0);
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_word_motion(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        motion: WordMotion,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let Some(metrics) = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id) else {
            return;
        };
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(runtime) =
            self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, copy_mode.pane_id)
        else {
            return;
        };
        let absolute_row =
            viewport_top_row(metrics).saturating_add(u32::from(copy_mode.cursor_row));
        let motion = match motion {
            WordMotion::NextStart => crate::pane::TerminalWordMotion::NextStart,
            WordMotion::PreviousStart => crate::pane::TerminalWordMotion::PreviousStart,
            WordMotion::NextEnd => crate::pane::TerminalWordMotion::NextEnd,
        };
        let Some(target) = runtime.word_motion_target(absolute_row, copy_mode.cursor_col, motion)
        else {
            return;
        };
        self.move_copy_cursor_to_absolute(terminal_runtimes, target, false);
    }

    fn copy_mode_paragraph(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, direction: i16) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let pane_id = copy_mode.pane_id;
        let Some(pane_height) = self
            .pane_info_by_id(pane_id)
            .map(|info| info.inner_rect.height)
        else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let limit = self
            .pane_scroll_metrics(terminal_runtimes, pane_id)
            .map(|metrics| metrics.max_offset_from_bottom + metrics.viewport_rows)
            .unwrap_or(pane_height as usize)
            .clamp(1, 1000);

        for _ in 0..limit {
            let before = self.copy_mode.as_ref().map(|copy_mode| {
                (
                    copy_mode.cursor_row,
                    copy_mode.cursor_col,
                    copy_mode.selection,
                )
            });
            let before_offset = self
                .pane_scroll_metrics(terminal_runtimes, pane_id)
                .map(|metrics| metrics.offset_from_bottom);

            self.move_copy_cursor(terminal_runtimes, direction, 0);

            let Some(after) = self.copy_mode.as_ref() else {
                return;
            };
            if self
                .copy_mode_visible_row_text(terminal_runtimes, after.cursor_row)
                .is_some_and(|text| text.trim().is_empty())
            {
                return;
            }

            let Some(after_metrics) = self.pane_scroll_metrics(terminal_runtimes, after.pane_id)
            else {
                continue;
            };
            let did_not_move = before
                == self.copy_mode.as_ref().map(|copy_mode| {
                    (
                        copy_mode.cursor_row,
                        copy_mode.cursor_col,
                        copy_mode.selection,
                    )
                })
                && before_offset == Some(after_metrics.offset_from_bottom);
            let at_top = direction < 0
                && after.cursor_row == 0
                && after_metrics.offset_from_bottom == after_metrics.max_offset_from_bottom;
            let at_bottom = direction > 0
                && after.cursor_row + 1 >= pane_height
                && after_metrics.offset_from_bottom == 0;
            if did_not_move || at_top || at_bottom {
                return;
            }
        }
    }

    fn copy_mode_visible_row_text(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        viewport_row: u16,
    ) -> Option<String> {
        let copy_mode = self.copy_mode.as_ref()?;
        let ws_idx = self.active?;
        let info = self.pane_info_by_id(copy_mode.pane_id)?;
        if viewport_row >= info.inner_rect.height || info.inner_rect.width == 0 {
            return None;
        }
        let metrics = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id);
        let row_selection = Selection::range(
            copy_mode.pane_id,
            viewport_row,
            0,
            info.inner_rect.width.saturating_sub(1),
            metrics,
        );
        self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, copy_mode.pane_id)?
            .extract_selection(&row_selection)
    }

    pub(crate) fn copy_mode_pane_is_focused(&self) -> bool {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return false;
        };
        self.current_pane_focus_target()
            .is_some_and(|target| target.pane_id == copy_mode.pane_id)
    }

    pub(crate) fn sync_copy_mode_with_focus(&mut self) {
        if self.copy_mode.is_none() {
            return;
        }
        if !matches!(
            self.mode,
            Mode::Copy | Mode::Terminal | Mode::Navigate | Mode::Prefix
        ) {
            return;
        }
        if self.copy_mode_pane_is_focused() {
            self.mode = Mode::Copy;
        } else if self.active.is_some() {
            self.clear_copy_mode_selection();
            self.mode = Mode::Terminal;
        } else {
            self.clear_copy_mode_selection();
            self.mode = Mode::Navigate;
        }
    }

    pub(crate) fn settle_terminal_mode_after_focus(&mut self) {
        self.mode = Mode::Terminal;
        self.sync_copy_mode_with_focus();
    }

    pub(crate) fn sync_copy_mode_search_geometry(&mut self) {
        let geometry = self.copy_mode.as_ref().and_then(|copy_mode| {
            self.view
                .pane_infos
                .iter()
                .find(|info| info.id == copy_mode.pane_id)
                .map(|info| (info.inner_rect.width, info.inner_rect.height))
        });
        let Some(copy_mode) = self.copy_mode.as_mut() else {
            return;
        };
        if let Some(geometry) = geometry {
            if copy_mode.search.geometry.is_some() && copy_mode.search.geometry != Some(geometry) {
                copy_mode.search.matches.clear();
                copy_mode.search.current = None;
            }
            copy_mode.search.geometry = Some(geometry);
        }
    }

    pub(crate) fn clear_copy_mode_selection(&mut self) {
        self.clear_selection();
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            copy_mode.selection = None;
        }
    }

    pub(crate) fn clear_copy_mode_for_removed_panes(
        &mut self,
        pane_ids: impl IntoIterator<Item = crate::layout::PaneId>,
    ) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        if pane_ids
            .into_iter()
            .any(|pane_id| pane_id == copy_mode.pane_id)
        {
            self.clear_selection();
            self.copy_mode = None;
            if self.mode == Mode::Copy {
                self.mode = if self.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
        }
    }

    fn sync_copy_mode_selection(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.as_ref() else {
            return;
        };
        let Some(selection) = copy_mode.selection else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id).cloned() else {
            return;
        };
        match selection {
            CopyModeSelection::Character => {
                let screen_col = info.inner_rect.x.saturating_add(copy_mode.cursor_col);
                let screen_row = info.inner_rect.y.saturating_add(copy_mode.cursor_row);
                self.update_selection_cursor(
                    terminal_runtimes,
                    copy_mode.pane_id,
                    screen_col,
                    screen_row,
                );
            }
            CopyModeSelection::Linewise { anchor_row } => {
                let metrics = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id);
                let cursor_row =
                    Selection::absolute_row_for_viewport(copy_mode.cursor_row, metrics);
                self.selection = Some(Selection::line_range(
                    copy_mode.pane_id,
                    anchor_row,
                    cursor_row,
                    info.inner_rect.width.saturating_sub(1),
                ));
            }
        }
    }
}

impl CopyModeSearchDirection {
    fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }
}

fn viewport_top_row(metrics: crate::pane::ScrollMetrics) -> u32 {
    metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom)
        .min(u32::MAX as usize) as u32
}

fn search_match_index(
    matches: &[crate::pane::TerminalTextMatch],
    direction: CopyModeSearchDirection,
    cursor: crate::pane::TerminalTextPoint,
    previous: Option<crate::pane::TerminalTextMatch>,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    match direction {
        CopyModeSearchDirection::Forward => {
            let origin = previous.map_or(cursor, |text_match| text_match.end);
            matches
                .iter()
                .position(|text_match| text_match.start > origin)
                .or(Some(0))
        }
        CopyModeSearchDirection::Backward => {
            let origin = previous.map_or(cursor, |text_match| text_match.start);
            matches
                .iter()
                .rposition(|text_match| text_match.end < origin)
                .or_else(|| matches.len().checked_sub(1))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordMotion {
    NextStart,
    PreviousStart,
    NextEnd,
}

fn first_non_blank_col(text: &str) -> Option<u16> {
    let mut col = 0u16;
    for ch in text.chars() {
        if !ch.is_whitespace() {
            return Some(col);
        }
        col = col.saturating_add(char_cell_width(ch));
    }
    None
}

fn last_character_col(text: &str) -> Option<u16> {
    let mut col = 0u16;
    let mut last_col = None;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        if width > 0 {
            last_col = Some(col);
            col = col.saturating_add(width);
        }
    }
    last_col
}

fn char_cell_width(ch: char) -> u16 {
    UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as u16
}

fn copy_mode_page_lines(height: u16, half_page: bool) -> usize {
    if height <= 2 {
        1
    } else if half_page {
        usize::from(height / 2)
    } else {
        usize::from(height - 2)
    }
}

fn copy_mode_command_char(key: TerminalKey) -> Option<char> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }

    if let Some(ch) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(ch);
    }

    let KeyCode::Char(ch) = key.code else {
        return None;
    };
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        Some(shifted_ascii_char(ch).unwrap_or(ch))
    } else {
        Some(ch)
    }
}

fn shifted_ascii_char(ch: char) -> Option<char> {
    match ch {
        'a'..='z' => Some(ch.to_ascii_uppercase()),
        '1' => Some('!'),
        '2' => Some('@'),
        '3' => Some('#'),
        '4' => Some('$'),
        '5' => Some('%'),
        '6' => Some('^'),
        '7' => Some('&'),
        '8' => Some('*'),
        '9' => Some('('),
        '0' => Some(')'),
        '-' => Some('_'),
        '=' => Some('+'),
        '[' => Some('{'),
        ']' => Some('}'),
        '\\' => Some('|'),
        ';' => Some(':'),
        '\'' => Some('"'),
        ',' => Some('<'),
        '.' => Some('>'),
        '/' => Some('?'),
        '`' => Some('~'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{app_for_mouse_test, numbered_lines_bytes};
    use super::*;
    use crate::{events::AppEvent, workspace::Workspace};
    use ratatui::layout::Rect;

    fn app_with_copy_runtime(
        runtime: impl FnOnce(u16, u16) -> crate::terminal::TerminalRuntime,
    ) -> (App, crate::layout::PaneId) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(0, 0, 20, 5));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            runtime(info.inner_rect.width, info.inner_rect.height),
        );
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        (app, pane_id)
    }

    fn app_with_copy_screen(bytes: &[u8]) -> (App, crate::layout::PaneId) {
        app_with_copy_runtime(|cols, rows| {
            crate::terminal::TerminalRuntime::test_with_screen_bytes(cols, rows, bytes)
        })
    }

    fn app_with_copy_scrollback(bytes: &[u8]) -> (App, crate::layout::PaneId) {
        app_with_copy_runtime(|cols, rows| {
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                cols,
                rows,
                16 * 1024,
                bytes,
            )
        })
    }

    fn app_with_split_copy_runtime(
        bytes: &[u8],
        first_runtime: impl FnOnce(u16, u16, &[u8]) -> crate::terminal::TerminalRuntime,
    ) -> (App, crate::layout::PaneId, crate::layout::PaneId) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(ratatui::layout::Direction::Horizontal);
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(0, 0, 40, 5));
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info");
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info");
        ws.tabs[0].runtimes.insert(
            first_pane,
            first_runtime(
                first_info.inner_rect.width,
                first_info.inner_rect.height,
                bytes,
            ),
        );
        ws.tabs[0].runtimes.insert(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width,
                second_info.inner_rect.height,
                b"",
            ),
        );
        ws.tabs[0].layout.focus_pane(first_pane);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        (app, first_pane, second_pane)
    }

    fn app_with_split_copy_screen(
        bytes: &[u8],
    ) -> (App, crate::layout::PaneId, crate::layout::PaneId) {
        app_with_split_copy_runtime(bytes, |cols, rows, bytes| {
            crate::terminal::TerminalRuntime::test_with_screen_bytes(cols, rows, bytes)
        })
    }

    fn app_with_split_copy_scrollback(
        bytes: &[u8],
    ) -> (App, crate::layout::PaneId, crate::layout::PaneId) {
        app_with_split_copy_runtime(bytes, |cols, rows, bytes| {
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                cols,
                rows,
                16 * 1024,
                bytes,
            )
        })
    }

    fn copy_mode_clipboard_text(app: &mut App) -> String {
        match app.event_rx.try_recv().expect("clipboard event") {
            AppEvent::ClipboardWrite { content } => {
                String::from_utf8(content).expect("utf8 clipboard")
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    fn copy_mode_viewport_top_row(app: &App, pane_id: crate::layout::PaneId) -> usize {
        let metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("copy mode scroll metrics");
        metrics
            .max_offset_from_bottom
            .saturating_sub(metrics.offset_from_bottom)
    }

    fn copy_mode_offset_from_bottom(app: &App, pane_id: crate::layout::PaneId) -> usize {
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("copy mode scroll metrics")
            .offset_from_bottom
    }

    fn copy_mode_scroll_metrics(
        app: &App,
        pane_id: crate::layout::PaneId,
    ) -> crate::pane::ScrollMetrics {
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("copy mode scroll metrics")
    }

    fn refresh_split_pane_infos(app: &mut App) {
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .expect("active tab")
            .layout
            .panes(Rect::new(0, 0, 40, 5));
    }

    #[tokio::test]
    async fn enter_copy_mode_tracks_focused_pane() {
        let (mut app, pane_id) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").pane_id,
            pane_id
        );
    }

    #[tokio::test]
    async fn copy_mode_ctrl_b_uses_page_up() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.prefix_code = KeyCode::Char('a');
        app.state.prefix_mods = KeyModifiers::CONTROL;
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let height = app.state.copy_mode.as_ref().expect("copy mode").cursor_row + 1;
        let expected_lines = copy_mode_page_lines(height, false);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL));

        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), expected_lines);
    }

    #[tokio::test]
    async fn copy_mode_prefix_takes_priority_over_ctrl_b_page_up() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;

        assert_eq!(app.state.mode, Mode::Prefix);
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
        assert!(app.state.copy_mode.is_some());
    }

    #[tokio::test]
    async fn copy_mode_prefix_escape_returns_to_copy_mode() {
        let (mut app, _) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode").clone();

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.copy_mode, Some(copy_mode));
    }

    #[tokio::test]
    async fn copy_mode_prefix_focus_keeps_copy_mode_on_source_pane() {
        let (mut app, first_pane, second_pane) = app_with_split_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode").clone();
        assert_eq!(copy_mode.pane_id, first_pane);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.copy_mode, Some(copy_mode.clone()));
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            second_pane
        );

        refresh_split_pane_infos(&mut app);
        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.copy_mode, Some(copy_mode));
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), first_pane);
    }

    #[tokio::test]
    async fn copy_mode_focus_away_preserves_scrollback_position() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, first_pane, second_pane) = app_with_split_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        let scrolled_offset = copy_mode_offset_from_bottom(&app, first_pane);
        assert!(scrolled_offset > 0);

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            second_pane
        );
        assert_eq!(
            copy_mode_offset_from_bottom(&app, first_pane),
            scrolled_offset
        );
    }

    #[tokio::test]
    async fn copy_mode_cancel_restores_scroll_after_workspace_switch() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state
            .workspaces
            .push(crate::workspace::Workspace::test_new("other"));
        app.state.enter_copy_mode(&app.terminal_runtimes);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        assert!(copy_mode_offset_from_bottom(&app, pane_id) > 0);

        app.state.switch_workspace(1);
        app.state.cancel_copy_mode(&app.terminal_runtimes);

        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
        assert!(app.state.copy_mode.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn copy_mode_clears_when_source_tab_closes_after_focus_away() {
        let (mut app, first_pane, _) = app_with_split_copy_screen(b"alpha\nbeta\n");
        let survivor_tab = app.state.workspaces[0].test_add_tab(Some("survivor"));
        let survivor_pane = app.state.workspaces[0].tabs[survivor_tab].root_pane;
        let survivor_terminal = app.state.workspaces[0].tabs[survivor_tab].panes[&survivor_pane]
            .attached_terminal_id
            .clone();
        app.state.terminals.insert(
            survivor_terminal.clone(),
            crate::terminal::TerminalState::new(survivor_terminal, "/tmp".into()),
        );
        app.state.enter_copy_mode(&app.terminal_runtimes);
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").pane_id,
            first_pane
        );

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()))
            .await;
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_some());

        assert!(!app.state.close_tab());

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn copy_mode_ctrl_f_uses_page_down() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let height = app.state.copy_mode.as_ref().expect("copy mode").cursor_row + 1;
        let page_lines = copy_mode_page_lines(height, false);
        app.state
            .set_pane_scroll_offset(&app.terminal_runtimes, pane_id, page_lines);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('f'), KeyModifiers::CONTROL));

        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
    }

    #[tokio::test]
    async fn copy_mode_line_end_stops_at_last_character() {
        let (mut app, _) = app_with_copy_screen(b"hello\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('$'), KeyModifiers::empty()));

        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            4
        );

        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_col = 0;
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::End, KeyModifiers::empty()));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            4
        );

        let (mut empty_app, _) = app_with_copy_screen(b"\r\n");
        empty_app
            .state
            .enter_copy_mode(&empty_app.terminal_runtimes);
        if let Some(copy_mode) = empty_app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 7;
        }
        empty_app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('$'), KeyModifiers::empty()));
        assert_eq!(
            empty_app
                .state
                .copy_mode
                .as_ref()
                .expect("copy mode")
                .cursor_col,
            0
        );

        let (mut wide_app, _) = app_with_copy_screen("a界\r\n".as_bytes());
        wide_app.state.enter_copy_mode(&wide_app.terminal_runtimes);
        if let Some(copy_mode) = wide_app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }
        wide_app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('$'), KeyModifiers::empty()));
        assert_eq!(
            wide_app
                .state
                .copy_mode
                .as_ref()
                .expect("copy mode")
                .cursor_col,
            1
        );
    }

    #[tokio::test]
    async fn copy_mode_word_motions_use_visible_row_words() {
        let (mut app, _) = app_with_copy_screen(b"foo bar baz\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('w'), KeyModifiers::empty()));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            4
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('e'), KeyModifiers::empty()));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            6
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::empty()));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            4
        );
    }

    fn submit_copy_search(app: &mut App, marker: char, query: &str) {
        app.handle_copy_mode_key(TerminalKey::new(
            KeyCode::Char(marker),
            KeyModifiers::empty(),
        ));
        for ch in query.chars() {
            app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char(ch), KeyModifiers::empty()));
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty()));
    }

    #[tokio::test]
    async fn copy_mode_search_wraps_and_repeats_in_both_directions() {
        let (mut app, pane_id) = app_with_copy_screen(b"alpha needle\r\nbeta needle\r\ngamma\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);

        submit_copy_search(&mut app, '/', "needle");

        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode.search.matches.len(), 2);
        assert_eq!(copy_mode.search.current, Some(0));
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            0
        );
        assert_eq!(copy_mode.cursor_col, 6);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty()));
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode.search.current, Some(1));
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            1
        );
        assert_eq!(copy_mode.cursor_col, 5);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode.search.current, Some(0));
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            0
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(
            app.state
                .copy_mode
                .as_ref()
                .expect("copy mode")
                .search
                .current,
            Some(1)
        );
    }

    #[tokio::test]
    async fn copy_mode_backward_search_uses_last_match_before_cursor() {
        let (mut app, pane_id) = app_with_copy_screen(b"alpha needle\r\nbeta needle\r\ngamma\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);

        submit_copy_search(&mut app, '?', "needle");

        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode.search.current, Some(1));
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            1
        );
    }

    #[tokio::test]
    async fn copy_mode_shifted_slash_opens_backward_search() {
        let (mut app, _) = app_with_copy_screen(b"alpha needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);

        app.handle_copy_mode_key(
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        );

        assert_eq!(
            app.state
                .copy_mode
                .as_ref()
                .and_then(|copy_mode| copy_mode.search.prompt.as_ref())
                .map(|prompt| prompt.direction),
            Some(CopyModeSearchDirection::Backward)
        );
    }

    #[tokio::test]
    async fn copy_mode_search_extends_an_active_selection() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::empty()));

        submit_copy_search(&mut app, '/', "needle");
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "alpha\nbeta n");
    }

    #[tokio::test]
    async fn copy_mode_search_prompt_escape_and_failure_preserve_copy_mode() {
        let (mut app, pane_id) = app_with_copy_screen(b"alpha\r\nbeta\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let before = (
            copy_mode_viewport_top_row(&app, pane_id),
            app.state.copy_mode.as_ref().expect("copy mode").cursor_row,
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('x'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::Copy);
        assert!(app
            .state
            .copy_mode
            .as_ref()
            .expect("copy mode")
            .search
            .prompt
            .is_none());

        submit_copy_search(&mut app, '/', "missing");
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode.search.query, "missing");
        assert!(copy_mode.search.matches.is_empty());
        assert_eq!(
            (
                copy_mode_viewport_top_row(&app, pane_id),
                copy_mode.cursor_row,
                copy_mode.cursor_col,
            ),
            before
        );
    }

    #[tokio::test]
    async fn copy_mode_escape_clears_search_before_exiting() {
        let (mut app, _) = app_with_copy_screen(b"alpha needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        submit_copy_search(&mut app, '/', "needle");

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));

        let search = &app.state.copy_mode.as_ref().expect("copy mode").search;
        assert!(search.query.is_empty());
        assert!(search.matches.is_empty());
        assert!(search.current.is_none());
        assert_eq!(app.state.mode, Mode::Copy);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
    }

    #[tokio::test]
    async fn copy_mode_escape_clears_selection_before_exiting() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::empty()));
        assert!(app.state.selection.is_some());

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::Copy);
        assert!(app.state.selection.is_none());
        assert!(app
            .state
            .copy_mode
            .as_ref()
            .expect("copy mode")
            .selection
            .is_none());

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn copy_mode_search_prompt_accepts_paste_without_exiting() {
        let (mut app, _) = app_with_copy_screen(b"alpha needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('/'), KeyModifiers::empty()));

        assert!(app.paste_into_active_text_input("needle"));
        assert_eq!(
            app.state
                .copy_mode
                .as_ref()
                .and_then(|copy_mode| copy_mode.search.prompt.as_ref())
                .map(|prompt| prompt.query.as_str()),
            Some("needle")
        );
        assert_eq!(app.state.mode, Mode::Copy);
    }

    #[tokio::test]
    async fn copy_mode_word_motions_cross_rows_and_respect_separators() {
        let (mut app, pane_id) = app_with_copy_screen(b"foo.bar_baz\r\nqux\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }

        for expected_col in [3, 4] {
            app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('w'), KeyModifiers::empty()));
            assert_eq!(
                app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
                expected_col
            );
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('w'), KeyModifiers::empty()));
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            1
        );
        assert_eq!(copy_mode.cursor_col, 0);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::empty()));
        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(
            copy_mode_viewport_top_row(&app, pane_id) + usize::from(copy_mode.cursor_row),
            0
        );
        assert_eq!(copy_mode.cursor_col, 4);
    }

    #[tokio::test]
    async fn copy_mode_search_does_not_change_live_follow_behavior() {
        let bytes = numbered_lines_bytes(32);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        submit_copy_search(&mut app, '/', "missing");
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);

        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("runtime")
            .test_process_pty_bytes(b"live output\r\n");
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);

        submit_copy_search(&mut app, '?', "000000");
        assert!(copy_mode_offset_from_bottom(&app, pane_id) > 0);
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        let visible_before = runtime.visible_text();
        runtime.test_process_pty_bytes(b"more output\r\n");
        assert_eq!(runtime.visible_text(), visible_before);
    }

    #[tokio::test]
    async fn copy_mode_resize_clears_matches_but_keeps_query() {
        let (mut app, _) = app_with_copy_screen(b"alpha needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        submit_copy_search(&mut app, '/', "needle");
        assert!(!app
            .state
            .copy_mode
            .as_ref()
            .expect("copy mode")
            .search
            .matches
            .is_empty());

        app.state.view.pane_infos[0].inner_rect.width = app.state.view.pane_infos[0]
            .inner_rect
            .width
            .saturating_sub(1);
        app.state.sync_copy_mode_search_geometry();

        let search = &app.state.copy_mode.as_ref().expect("copy mode").search;
        assert_eq!(search.query, "needle");
        assert!(search.matches.is_empty());
        assert!(search.current.is_none());
    }

    #[tokio::test]
    async fn copy_mode_hidden_source_does_not_look_like_a_resize() {
        let (mut app, _) = app_with_copy_screen(b"alpha needle\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        submit_copy_search(&mut app, '/', "needle");
        let before = app
            .state
            .copy_mode
            .as_ref()
            .expect("copy mode")
            .search
            .clone();

        app.state.view.pane_infos.clear();
        app.state.sync_copy_mode_search_geometry();

        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").search,
            before
        );
    }

    #[tokio::test]
    async fn copy_mode_shift_v_y_copies_visible_line() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 1;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "beta");
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn copy_mode_shift_v_extends_linewise_down() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta\r\ngamma\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "alpha\nbeta");
    }

    #[tokio::test]
    async fn copy_mode_shift_v_extends_linewise_up() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta\r\ngamma\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 1;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "alpha\nbeta");
    }

    #[tokio::test]
    async fn copy_mode_shift_v_reverses_without_character_tail() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta\r\ngamma\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 1;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('k'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "alpha\nbeta");
    }

    #[tokio::test]
    async fn copy_mode_shift_v_horizontal_motion_keeps_linewise_selection() {
        let (mut app, _) = app_with_copy_screen(b"alpha\r\nbeta\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 1;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert_eq!(copy_mode_clipboard_text(&mut app), "beta");
    }

    #[tokio::test]
    async fn copy_mode_shift_v_page_up_keeps_linewise_scrollback_selection() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 2;
        }

        let anchor_row = copy_mode_viewport_top_row(&app, pane_id);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        let cursor_row = copy_mode_viewport_top_row(&app, pane_id);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        assert!(cursor_row < anchor_row);
        let expected = (cursor_row..=anchor_row)
            .map(|row| format!("{row:06}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(copy_mode_clipboard_text(&mut app), expected);
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
    }

    #[tokio::test]
    async fn copy_mode_page_up_uses_tmux_page_size() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let height = app.state.copy_mode.as_ref().expect("copy mode").cursor_row + 1;
        let expected_lines = copy_mode_page_lines(height, false);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));

        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), expected_lines);
    }

    #[tokio::test]
    async fn copy_mode_ctrl_u_moves_cursor_when_history_top_clamps() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let bottom = app.state.copy_mode.as_ref().expect("copy mode").cursor_row;
        let lines = copy_mode_page_lines(bottom + 1, true);
        let metrics = copy_mode_scroll_metrics(&app, pane_id);
        assert!(metrics.max_offset_from_bottom >= lines);
        app.state.set_pane_scroll_offset(
            &app.terminal_runtimes,
            pane_id,
            metrics.max_offset_from_bottom - lines + 1,
        );
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = bottom;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        let expected_cursor_delta = 1;
        assert_eq!(
            copy_mode_offset_from_bottom(&app, pane_id),
            metrics.max_offset_from_bottom
        );
        assert_eq!(
            copy_mode.cursor_row,
            bottom.saturating_sub(expected_cursor_delta as u16)
        );
    }

    #[tokio::test]
    async fn copy_mode_ctrl_d_moves_cursor_when_live_bottom_clamps() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);
        let bottom = app.state.copy_mode.as_ref().expect("copy mode").cursor_row;
        let lines = copy_mode_page_lines(bottom + 1, true);
        assert!(lines > 1);
        app.state
            .set_pane_scroll_offset(&app.terminal_runtimes, pane_id, lines - 1);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('d'), KeyModifiers::CONTROL));

        let copy_mode = app.state.copy_mode.as_ref().expect("copy mode");
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
        assert_eq!(copy_mode.cursor_row, 1);
    }

    #[tokio::test]
    async fn copy_mode_q_exits_and_returns_to_bottom_after_scrollback() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        app.state.enter_copy_mode(&app.terminal_runtimes);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        assert!(copy_mode_offset_from_bottom(&app, pane_id) > 0);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('q'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), 0);
    }

    #[tokio::test]
    async fn copy_mode_q_restores_entry_scrollback_offset() {
        let bytes = numbered_lines_bytes(64);
        let (mut app, pane_id) = app_with_copy_scrollback(&bytes);
        let entry_offset = 3;
        app.state
            .set_pane_scroll_offset(&app.terminal_runtimes, pane_id, entry_offset);
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), entry_offset);

        app.state.enter_copy_mode(&app.terminal_runtimes);
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::PageUp, KeyModifiers::empty()));
        assert!(copy_mode_offset_from_bottom(&app, pane_id) > entry_offset);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('q'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
        assert_eq!(copy_mode_offset_from_bottom(&app, pane_id), entry_offset);
    }

    #[tokio::test]
    async fn shifted_punctuation_keys_work_with_enhanced_key_reporting() {
        let (mut app, _) = app_with_copy_screen(b"foo\r\n\r\nbar\r\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 2;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('6'), KeyModifiers::SHIFT));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_col,
            0
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_row,
            3
        );

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('['), KeyModifiers::SHIFT));
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_row,
            1
        );

        app.handle_copy_mode_key(
            TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('}' as u32),
        );
        assert_eq!(
            app.state.copy_mode.as_ref().expect("copy mode").cursor_row,
            3
        );
    }

    #[tokio::test]
    async fn copy_mode_v_y_copies_selection_and_exits() {
        let (mut app, _) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        match app.event_rx.try_recv().expect("clipboard event") {
            AppEvent::ClipboardWrite { content } => assert_eq!(content, b"alp"),
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
    }

    #[tokio::test]
    async fn copy_mode_same_tab_switch_preserves_selection() {
        let (mut app, _) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 0;
        }
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('l'), KeyModifiers::empty()));

        assert!(app.state.switch_workspace_tab(0, 0));

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));
        assert_eq!(copy_mode_clipboard_text(&mut app), "alp");
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.copy_mode.is_none());
    }
}
