use std::time::Instant;

use super::{App, SESSION_SAVE_DEBOUNCE};

impl App {
    pub(super) fn schedule_session_save(&mut self) {
        if !self.no_session {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        if self.state.session_dirty {
            self.state.session_dirty = false;
            self.schedule_session_save();
        }
    }

    pub(crate) fn save_session_now(&mut self) {
        if self.no_session {
            self.session_save_deadline = None;
            return;
        }

        if self.state.workspaces.is_empty() {
            crate::persist::clear();
        } else {
            let snap = crate::persist::capture(
                &self.state.workspaces,
                self.state.active,
                self.state.selected,
                self.state.agent_panel_scope,
                self.state.sidebar_width,
                self.state.sidebar_section_split,
            );
            crate::persist::save(&snap);
        }

        self.session_save_deadline = None;
    }
}
