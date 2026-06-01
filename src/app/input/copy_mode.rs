use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::{state::CopyModeState, App, AppState, Mode},
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

        self.clear_selection();
        self.copy_mode = Some(CopyModeState {
            pane_id,
            cursor_row: cursor.0.min(info.inner_rect.height.saturating_sub(1)),
            cursor_col: cursor.1.min(info.inner_rect.width.saturating_sub(1)),
            selecting: false,
        });
        self.mode = Mode::Copy;
    }

    pub(crate) fn handle_copy_mode_key(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        key: TerminalKey,
    ) {
        match key.code {
            KeyCode::Esc => {
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
            'w' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::NextStart),
            'b' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::PreviousStart),
            'e' => self.copy_mode_word_motion(terminal_runtimes, WordMotion::NextEnd),
            '{' => self.copy_mode_paragraph(terminal_runtimes, -1),
            '}' => self.copy_mode_paragraph(terminal_runtimes, 1),
            _ => {}
        }
    }

    fn exit_copy_mode(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, copy: bool) {
        if copy {
            self.copy_selection(terminal_runtimes);
        } else {
            self.clear_selection();
        }
        self.copy_mode = None;
        self.mode = if self.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    fn begin_copy_mode_selection(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode else {
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
            copy_mode.selecting = true;
        }
    }

    fn select_copy_mode_line(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id) else {
            return;
        };
        let end_col = info.inner_rect.width.saturating_sub(1);
        let metrics = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id);
        self.selection = Some(Selection::range(
            copy_mode.pane_id,
            copy_mode.cursor_row,
            0,
            end_col,
            metrics,
        ));
        copy_mode.cursor_col = end_col;
        copy_mode.selecting = true;
        self.copy_mode = Some(copy_mode);
    }

    fn move_copy_cursor(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        row_delta: i16,
        col_delta: i16,
    ) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id).cloned() else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };

        if col_delta < 0 {
            copy_mode.cursor_col = copy_mode
                .cursor_col
                .saturating_sub(col_delta.unsigned_abs());
        } else if col_delta > 0 {
            copy_mode.cursor_col = copy_mode
                .cursor_col
                .saturating_add(col_delta as u16)
                .min(info.inner_rect.width.saturating_sub(1));
        }

        if row_delta < 0 {
            let delta = row_delta.unsigned_abs();
            if copy_mode.cursor_row >= delta {
                copy_mode.cursor_row -= delta;
            } else {
                self.scroll_pane_up(terminal_runtimes, copy_mode.pane_id, usize::from(delta));
                copy_mode.cursor_row = 0;
            }
        } else if row_delta > 0 {
            let delta = row_delta as u16;
            let bottom = info.inner_rect.height.saturating_sub(1);
            if copy_mode.cursor_row.saturating_add(delta) <= bottom {
                copy_mode.cursor_row += delta;
            } else {
                self.scroll_pane_down(terminal_runtimes, copy_mode.pane_id, usize::from(delta));
                copy_mode.cursor_row = bottom;
            }
        }

        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn scroll_copy_mode_page(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        direction: i16,
        half_page: bool,
    ) {
        let Some(copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let lines = if half_page {
            (info.inner_rect.height / 2).max(1)
        } else {
            info.inner_rect.height.max(1)
        } as usize;
        if direction < 0 {
            self.scroll_pane_up(terminal_runtimes, copy_mode.pane_id, lines);
        } else {
            self.scroll_pane_down(terminal_runtimes, copy_mode.pane_id, lines);
        }
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_history_top(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(metrics) = self.pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id) else {
            return;
        };
        self.set_pane_scroll_offset(
            terminal_runtimes,
            copy_mode.pane_id,
            metrics.max_offset_from_bottom,
        );
        copy_mode.cursor_row = 0;
        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_history_bottom(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        self.set_pane_scroll_offset(terminal_runtimes, copy_mode.pane_id, 0);
        copy_mode.cursor_row = info.inner_rect.height.saturating_sub(1);
        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_line_edge(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, end: bool) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        copy_mode.cursor_col = if end {
            info.inner_rect.width.saturating_sub(1)
        } else {
            0
        };
        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_first_non_blank(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(text) = self.copy_mode_visible_row_text(terminal_runtimes, copy_mode.cursor_row)
        else {
            return;
        };
        copy_mode.cursor_col = first_non_blank_col(&text).unwrap_or(0);
        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_word_motion(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        motion: WordMotion,
    ) {
        let Some(mut copy_mode) = self.copy_mode else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id) else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let Some(text) = self.copy_mode_visible_row_text(terminal_runtimes, copy_mode.cursor_row)
        else {
            return;
        };
        let Some(col) = word_motion_target(&text, copy_mode.cursor_col, motion) else {
            return;
        };
        copy_mode.cursor_col = col.min(info.inner_rect.width.saturating_sub(1));
        self.copy_mode = Some(copy_mode);
        self.sync_copy_mode_selection(terminal_runtimes);
    }

    fn copy_mode_paragraph(&mut self, terminal_runtimes: &TerminalRuntimeRegistry, direction: i16) {
        let Some(copy_mode) = self.copy_mode else {
            return;
        };
        let Some(pane_height) = self
            .pane_info_by_id(copy_mode.pane_id)
            .map(|info| info.inner_rect.height)
        else {
            self.exit_copy_mode(terminal_runtimes, false);
            return;
        };
        let limit = self
            .pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id)
            .map(|metrics| metrics.max_offset_from_bottom + metrics.viewport_rows)
            .unwrap_or(pane_height as usize)
            .clamp(1, 1000);

        for _ in 0..limit {
            let before = self.copy_mode;
            let before_offset = self
                .pane_scroll_metrics(terminal_runtimes, copy_mode.pane_id)
                .map(|metrics| metrics.offset_from_bottom);

            self.move_copy_cursor(terminal_runtimes, direction, 0);

            let Some(after) = self.copy_mode else {
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
            let did_not_move =
                before == self.copy_mode && before_offset == Some(after_metrics.offset_from_bottom);
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
        let copy_mode = self.copy_mode?;
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

    fn sync_copy_mode_selection(&mut self, terminal_runtimes: &TerminalRuntimeRegistry) {
        let Some(copy_mode) = self.copy_mode.filter(|copy_mode| copy_mode.selecting) else {
            return;
        };
        let Some(info) = self.pane_info_by_id(copy_mode.pane_id).cloned() else {
            return;
        };
        let screen_col = info.inner_rect.x.saturating_add(copy_mode.cursor_col);
        let screen_row = info.inner_rect.y.saturating_add(copy_mode.cursor_row);
        self.update_selection_cursor(terminal_runtimes, copy_mode.pane_id, screen_col, screen_row);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordMotion {
    NextStart,
    PreviousStart,
    NextEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WordSpan {
    start: u16,
    end: u16,
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

fn word_motion_target(text: &str, cursor_col: u16, motion: WordMotion) -> Option<u16> {
    let spans = word_spans(text);
    match motion {
        WordMotion::NextStart => spans.iter().enumerate().find_map(|(idx, span)| {
            if cursor_col < span.start {
                Some(span.start)
            } else if cursor_col >= span.start && cursor_col <= span.end {
                spans.get(idx + 1).map(|next| next.start)
            } else {
                None
            }
        }),
        WordMotion::PreviousStart => spans
            .iter()
            .rev()
            .find(|span| span.start < cursor_col)
            .map(|span| span.start),
        WordMotion::NextEnd => spans.iter().find_map(|span| {
            if cursor_col < span.end {
                Some(span.end)
            } else {
                None
            }
        }),
    }
}

fn word_spans(text: &str) -> Vec<WordSpan> {
    let mut spans = Vec::new();
    let mut col = 0u16;
    let mut start = None;

    for ch in text.chars() {
        let width = char_cell_width(ch);
        if ch.is_whitespace() {
            if let Some(start_col) = start.take() {
                spans.push(WordSpan {
                    start: start_col,
                    end: col.saturating_sub(1),
                });
            }
        } else if start.is_none() {
            start = Some(col);
        }
        col = col.saturating_add(width);
    }

    if let Some(start_col) = start {
        spans.push(WordSpan {
            start: start_col,
            end: col.saturating_sub(1),
        });
    }
    spans
}

fn char_cell_width(ch: char) -> u16 {
    UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as u16
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
    use super::super::app_for_mouse_test;
    use super::*;
    use crate::{events::AppEvent, workspace::Workspace};
    use ratatui::layout::Rect;

    fn app_with_copy_screen(bytes: &[u8]) -> (App, crate::layout::PaneId) {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(0, 0, 20, 5));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
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
        (app, pane_id)
    }

    #[tokio::test]
    async fn enter_copy_mode_tracks_focused_pane() {
        let (mut app, pane_id) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.copy_mode.expect("copy mode").pane_id, pane_id);
    }

    #[tokio::test]
    async fn copy_mode_ignores_prefix_key() {
        let (mut app, _) = app_with_copy_screen(b"foo bar\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 0;
            copy_mode.cursor_col = 4;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::CONTROL));

        let copy_mode = app.state.copy_mode.expect("copy mode");
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(copy_mode.cursor_col, 4);
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
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_col, 4);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('e'), KeyModifiers::empty()));
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_col, 6);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('b'), KeyModifiers::empty()));
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_col, 4);
    }

    #[tokio::test]
    async fn copy_mode_shift_v_y_copies_visible_line() {
        let (mut app, _) = app_with_copy_screen(b"alpha\nbeta\n");
        app.state.enter_copy_mode(&app.terminal_runtimes);
        if let Some(copy_mode) = app.state.copy_mode.as_mut() {
            copy_mode.cursor_row = 1;
            copy_mode.cursor_col = 2;
        }

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('v'), KeyModifiers::SHIFT));
        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('y'), KeyModifiers::empty()));

        match app.event_rx.try_recv().expect("clipboard event") {
            AppEvent::ClipboardWrite { content } => {
                let text = String::from_utf8(content).expect("utf8 clipboard");
                assert!(
                    text.ends_with("beta"),
                    "copied line should include beta: {text:?}"
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert_eq!(app.state.mode, Mode::Terminal);
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
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_col, 0);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT));
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_row, 3);

        app.handle_copy_mode_key(TerminalKey::new(KeyCode::Char('['), KeyModifiers::SHIFT));
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_row, 1);

        app.handle_copy_mode_key(
            TerminalKey::new(KeyCode::Char(']'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('}' as u32),
        );
        assert_eq!(app.state.copy_mode.expect("copy mode").cursor_row, 3);
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
}
