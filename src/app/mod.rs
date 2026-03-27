//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

mod actions;
mod input;
pub mod state;

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::config::Config;
use crate::events::AppEvent;
use crate::workspace::Workspace;

pub use state::{AppState, Mode, ViewState, CONTEXT_MENU_ITEMS};

/// Full application: AppState + runtime concerns (event channels, async I/O).
pub struct App {
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    event_rx: mpsc::Receiver<AppEvent>,
    no_session: bool,
}

impl App {
    pub fn new(config: &Config, no_session: bool) -> Self {
        let (prefix_code, prefix_mods) = config.prefix_key();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>(64);

        // Try to restore previous session
        let (workspaces, active, selected) = if no_session {
            (Vec::new(), None, 0)
        } else if let Some(snap) = crate::persist::load() {
            let ws = crate::persist::restore(&snap, 24, 80, event_tx.clone());
            if ws.is_empty() {
                info!("session file found but no workspaces restored");
                (Vec::new(), None, 0)
            } else {
                info!(count = ws.len(), "session restored");
                let active = snap.active.filter(|&i| i < ws.len());
                let selected = snap.selected.min(ws.len().saturating_sub(1));
                (ws, active, selected)
            }
        } else {
            (Vec::new(), None, 0)
        };

        let mode = if active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        };

        let state = AppState {
            workspaces,
            active,
            selected,
            mode,
            should_quit: false,
            name_input: String::new(),
            view: state::ViewState {
                sidebar_rect: Rect::default(),
                terminal_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            selection: None,
            context_menu: None,
            update_available: None,
            update_dismissed: false,
            prefix_code,
            prefix_mods,
            prefix_label: config.prefix_label(),
            sidebar_width: config.ui.sidebar_width,
            sidebar_collapsed: false,
            confirm_close: config.ui.confirm_close,
            accent: crate::config::parse_color(&config.ui.accent),
            sound: config.ui.sound,
            keybinds: config.keybinds(),
        };

        // Background auto-update (skipped in --no-session / test mode)
        if !no_session {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }

        Self {
            state,
            event_tx,
            event_rx,
            no_session,
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.state.should_quit {
            terminal.draw(|frame| {
                crate::ui::compute_view(&mut self.state, frame.area());
                crate::ui::render(&self.state, frame);
            })?;

            // Drain internal events
            while let Ok(ev) = self.event_rx.try_recv() {
                self.state.handle_app_event(ev);
            }

            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key).await;
                    }
                    Event::Paste(text) => self.handle_paste(text).await,
                    Event::Mouse(mouse) => self.state.handle_mouse(mouse),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }

        // Save session on exit (skip in --no-session mode)
        if !self.no_session && !self.state.workspaces.is_empty() {
            let snap = crate::persist::capture(
                &self.state.workspaces,
                self.state.active,
                self.state.selected,
            );
            crate::persist::save(&snap);
        }

        Ok(())
    }

    /// Create a workspace with a real PTY (needs event_tx).
    fn create_workspace(&mut self, name: String) {
        let (rows, cols) = self.state.estimate_pane_size();
        match Workspace::new(name, rows, cols, self.event_tx.clone()) {
            Ok(ws) => {
                self.state.workspaces.push(ws);
                let idx = self.state.workspaces.len() - 1;
                self.state.switch_workspace(idx);
                self.state.mode = Mode::Terminal;
            }
            Err(e) => {
                error!(err = %e, "failed to create workspace");
                self.state.mode = Mode::Navigate;
            }
        }
    }
}
