use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::{PaneId, TileLayout};
use crate::pane::{PaneRuntime, PaneState};

pub struct Tab {
    pub custom_name: Option<String>,
    pub number: usize,
    /// Identity source for this tab's pane tree.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    /// Pane state — always present, testable without PTYs.
    pub panes: HashMap<PaneId, PaneState>,
    /// Best-effort cwd cache per pane. Live runtime cwd wins when available.
    pub pane_cwds: HashMap<PaneId, PathBuf>,
    /// Pane runtimes — only present in production (empty in tests).
    pub runtimes: HashMap<PaneId, PaneRuntime>,
    pub zoomed: bool,
    pub events: mpsc::Sender<AppEvent>,
    pub(crate) render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<AtomicBool>,
}

impl Drop for Tab {
    fn drop(&mut self) {
        let runtimes = std::mem::take(&mut self.runtimes);
        for (pane_id, runtime) in runtimes {
            runtime.shutdown(pane_id);
        }
    }
}

impl Tab {
    pub fn new(
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let (layout, root_id) = TileLayout::new();
        let runtime = PaneRuntime::spawn(
            root_id,
            rows,
            cols,
            initial_cwd.clone(),
            scrollback_limit_bytes,
            host_terminal_theme,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        )?;

        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let mut pane_cwds = HashMap::new();
        pane_cwds.insert(root_id, initial_cwd);
        let mut runtimes = HashMap::new();
        runtimes.insert(root_id, runtime);

        Ok(Self {
            custom_name: None,
            number,
            root_pane: root_id,
            layout,
            panes,
            pane_cwds,
            runtimes,
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        })
    }

    pub fn display_name(&self) -> String {
        self.custom_name
            .clone()
            .unwrap_or_else(|| self.number.to_string())
    }

    pub fn is_auto_named(&self) -> bool {
        self.custom_name.is_none()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
    ) -> std::io::Result<PaneId> {
        self.split_focused_with_runtime(
            direction,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            None,
        )
    }

    pub fn split_focused_command(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        command: &str,
        extra_env: &[(String, String)],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
    ) -> std::io::Result<PaneId> {
        self.split_focused_with_runtime(
            direction,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            Some((command, extra_env)),
        )
    }

    fn split_focused_with_runtime(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        command: Option<(&str, &[(String, String)])>,
    ) -> std::io::Result<PaneId> {
        let new_id = self.layout.split_focused(direction);
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let runtime = if let Some((command, extra_env)) = command {
            PaneRuntime::spawn_shell_command(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                command,
                extra_env,
                scrollback_limit_bytes,
                host_terminal_theme,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            )?
        } else {
            PaneRuntime::spawn(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            )?
        };
        self.panes.insert(new_id, PaneState::new());
        self.pane_cwds.insert(new_id, actual_cwd);
        self.runtimes.insert(new_id, runtime);
        self.zoomed = false;
        Ok(new_id)
    }

    pub fn close_focused(&mut self) -> Option<(PaneId, Option<PaneRuntime>)> {
        let pane_id = self.layout.focused();
        self.detach_pane(pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
        self.detach_pane(pane_id)
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
        self.detach_pane(pane_id)
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
        if self.layout.pane_count() <= 1 {
            return None;
        }

        let next_root = self.promoted_root_if_needed(pane_id);

        if self.layout.focused() == pane_id {
            self.layout.close_focused();
        } else {
            let prev_focus = self.layout.focused();
            self.layout.focus_pane(pane_id);
            self.layout.close_focused();
            self.layout.focus_pane(prev_focus);
        }

        self.panes.remove(&pane_id);
        self.pane_cwds.remove(&pane_id);
        let runtime = self.runtimes.remove(&pane_id);
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some((pane_id, runtime))
    }

    fn promoted_root_if_needed(&self, closing: PaneId) -> Option<PaneId> {
        if self.root_pane != closing {
            return None;
        }
        self.layout.pane_ids().into_iter().find(|id| *id != closing)
    }

    pub fn focused_runtime(&self) -> Option<&PaneRuntime> {
        self.runtimes.get(&self.layout.focused())
    }

    pub fn cwd_for_pane(&self, pane_id: PaneId) -> Option<PathBuf> {
        self.runtimes
            .get(&pane_id)
            .and_then(|rt| rt.cwd())
            .or_else(|| self.pane_cwds.get(&pane_id).cloned())
    }

    pub fn root_cwd(&self) -> Option<PathBuf> {
        self.cwd_for_pane(self.root_pane)
    }

    #[cfg(test)]
    #[allow(dead_code)] // retained for focused layout-order assertions in tests
    pub fn pane_states(&self) -> Vec<(crate::detect::AgentState, bool)> {
        self.layout
            .pane_ids()
            .iter()
            .map(|id| {
                self.panes
                    .get(id)
                    .map(|p| (p.state, p.seen))
                    .unwrap_or((crate::detect::AgentState::Unknown, true))
            })
            .collect()
    }
}
