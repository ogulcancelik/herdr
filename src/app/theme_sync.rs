use std::sync::atomic::Ordering;

use super::App;

impl App {
    pub(super) fn query_host_terminal_theme(&self) {
        use std::io::Write;

        let _ = std::io::stdout()
            .write_all(crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes());
        let _ = std::io::stdout().flush();
    }

    pub(super) fn update_host_terminal_theme(
        &mut self,
        kind: crate::terminal_theme::DefaultColorKind,
        color: crate::terminal_theme::RgbColor,
    ) -> bool {
        let next_theme = self.state.host_terminal_theme.with_color(kind, color);
        self.set_host_terminal_theme(next_theme)
    }

    pub(crate) fn set_host_terminal_theme(
        &mut self,
        theme: crate::terminal_theme::TerminalTheme,
    ) -> bool {
        if theme.is_empty() || theme == self.state.host_terminal_theme {
            return false;
        }
        self.state.host_terminal_theme = theme;
        self.apply_host_terminal_theme_to_panes();
        true
    }

    fn apply_host_terminal_theme_to_panes(&self) {
        if self.state.host_terminal_theme.is_empty() {
            return;
        }

        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for runtime in tab.runtimes.values() {
                    runtime.apply_host_terminal_theme(self.state.host_terminal_theme);
                }
            }
        }

        self.render_dirty.store(true, Ordering::Release);
        self.render_notify.notify_one();
    }
}
