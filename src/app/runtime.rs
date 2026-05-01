use std::time::{Duration, Instant};

use crossterm::terminal;

use super::{
    auto_updates_enabled, repeat_key_identity, App, Mode, ANIMATION_INTERVAL,
    AUTO_UPDATE_CHECK_INTERVAL, GIT_REMOTE_STATUS_REFRESH_INTERVAL, MIN_RENDER_INTERVAL,
    RESIZE_POLL_INTERVAL,
};
use crate::workspace::Workspace;

impl App {
    pub(crate) fn drain_api_requests(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.api_rx.try_recv() {
            changed |= self.handle_api_request_message(msg);
        }
        changed
    }

    pub(super) fn handle_api_request_message(
        &mut self,
        msg: crate::api::ApiRequestMessage,
    ) -> bool {
        let changed = crate::api::request_changes_ui(&msg.request);
        let response = self.handle_api_request(msg.request);
        let _ = msg.respond_to.send(response);
        changed
    }

    pub(super) async fn handle_raw_input_batch(
        &mut self,
        first: crate::raw_input::RawInputEvent,
    ) -> bool {
        let mut changed = self.handle_raw_input_event(first).await;

        while let Some(rx) = self.input_rx.as_mut() {
            match rx.try_recv() {
                Ok(event) => changed |= self.handle_raw_input_event(event).await,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.input_rx = None;
                    break;
                }
            }
        }

        changed
    }

    pub(super) async fn handle_raw_input_event(
        &mut self,
        event: crate::raw_input::RawInputEvent,
    ) -> bool {
        match event {
            crate::raw_input::RawInputEvent::Key(key) => {
                let key_id = repeat_key_identity(&key);
                match key.kind {
                    crossterm::event::KeyEventKind::Press => {
                        if self.state.mode == Mode::Terminal {
                            self.suppressed_repeat_keys.remove(&key_id);
                        } else {
                            self.suppressed_repeat_keys.insert(key_id);
                        }
                        self.handle_key(key).await;
                        true
                    }
                    crossterm::event::KeyEventKind::Repeat => {
                        if self.state.mode == Mode::Terminal
                            && !self.suppressed_repeat_keys.contains(&key_id)
                        {
                            self.handle_key(key).await;
                            true
                        } else {
                            false
                        }
                    }
                    crossterm::event::KeyEventKind::Release => {
                        self.suppressed_repeat_keys.remove(&key_id);
                        false
                    }
                }
            }
            crate::raw_input::RawInputEvent::Paste(text) => {
                self.handle_paste(text).await;
                true
            }
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                self.handle_mouse(mouse);
                true
            }
            crate::raw_input::RawInputEvent::OuterFocusGained => {
                self.state.outer_terminal_focus = Some(true);
                self.state.mark_active_tab_seen()
            }
            crate::raw_input::RawInputEvent::OuterFocusLost => {
                self.state.outer_terminal_focus = Some(false);
                false
            }
            crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                self.update_host_terminal_theme(kind, color)
            }
            crate::raw_input::RawInputEvent::Unsupported => false,
        }
    }

    fn handle_resize_poll(&mut self) -> bool {
        let Ok(size) = terminal::size() else {
            return false;
        };
        if self.last_terminal_size != Some(size) {
            self.last_terminal_size = Some(size);
            return true;
        }
        false
    }

    pub(crate) fn handle_scheduled_tasks(&mut self, now: Instant) -> bool {
        let mut changed = false;

        self.sync_animation_timer(now);

        if now >= self.next_resize_poll {
            changed |= self.handle_resize_poll();
            self.next_resize_poll = now + RESIZE_POLL_INTERVAL;
        }

        if self
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.config_diagnostic_deadline = None;
            self.state.config_diagnostic = None;
            changed = true;
        }

        if self.toast_deadline.is_some_and(|deadline| now >= deadline) {
            self.toast_deadline = None;
            self.state.toast = None;
            changed = true;
        }

        if self
            .next_animation_tick
            .is_some_and(|deadline| now >= deadline)
        {
            self.state.spinner_tick = self.state.spinner_tick.wrapping_add(1);
            self.next_animation_tick = Some(now + ANIMATION_INTERVAL);
            changed = true;
        }

        if self
            .git_refresh_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            for ws in &mut self.state.workspaces {
                ws.refresh_git_ahead_behind();
            }
            self.last_git_remote_status_refresh = now;
            changed = true;
        }

        if self
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.run_auto_update_check();
        }

        if self
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.save_session_now();
        }

        self.sync_animation_timer(now);
        changed
    }

    pub(crate) fn sync_animation_timer(&mut self, now: Instant) {
        self.sync_animation_timer_with_interval(now, ANIMATION_INTERVAL);
    }

    pub(crate) fn sync_headless_animation_timer(&mut self, now: Instant) {
        self.sync_animation_timer_with_interval(now, crate::app::HEADLESS_ANIMATION_INTERVAL);
    }

    fn sync_animation_timer_with_interval(&mut self, now: Instant, interval: Duration) {
        if self.agent_panel_has_animation() {
            self.next_animation_tick.get_or_insert(now + interval);
        } else {
            self.next_animation_tick = None;
        }
    }

    fn agent_panel_has_animation(&self) -> bool {
        match self.state.agent_panel_scope {
            crate::app::state::AgentPanelScope::CurrentWorkspace => self
                .state
                .active
                .and_then(|idx| self.state.workspaces.get(idx))
                .is_some_and(Workspace::has_working_pane),
            crate::app::state::AgentPanelScope::AllWorkspaces => self
                .state
                .workspaces
                .iter()
                .any(Workspace::has_working_pane),
        }
    }

    pub(crate) fn can_render_now(&self, now: Instant) -> bool {
        match self.last_render_at {
            Some(last_render_at) => now.duration_since(last_render_at) >= MIN_RENDER_INTERVAL,
            None => true,
        }
    }

    pub(crate) fn run_auto_update_check(&mut self) {
        if !auto_updates_enabled(self.no_session) {
            self.next_auto_update_check = None;
            return;
        }

        self.next_auto_update_check = self
            .state
            .update_available
            .is_none()
            .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL);

        if self.state.update_available.is_some() {
            return;
        }

        let update_tx = self.event_tx.clone();
        std::thread::spawn(move || crate::update::auto_update(update_tx));
    }

    pub(crate) fn git_refresh_deadline(&self) -> Option<Instant> {
        (!self.state.workspaces.is_empty())
            .then_some(self.last_git_remote_status_refresh + GIT_REMOTE_STATUS_REFRESH_INTERVAL)
    }

    pub(crate) fn next_loop_deadline(&self, now: Instant, needs_render: bool) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, true)
    }

    pub(crate) fn next_headless_loop_deadline(
        &self,
        now: Instant,
        needs_render: bool,
    ) -> Option<Instant> {
        self.next_loop_deadline_with_resize_poll(now, needs_render, false)
    }

    fn next_loop_deadline_with_resize_poll(
        &self,
        now: Instant,
        needs_render: bool,
        include_resize_poll: bool,
    ) -> Option<Instant> {
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            include_resize_poll.then_some(self.next_resize_poll),
            self.config_diagnostic_deadline,
            self.toast_deadline,
            self.next_animation_tick,
            self.git_refresh_deadline(),
            self.next_auto_update_check,
            self.session_save_deadline,
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(crate) fn drain_internal_events(&mut self) -> bool {
        let mut had_event = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            had_event = true;
            self.handle_internal_event(ev);
        }
        had_event
    }
}
