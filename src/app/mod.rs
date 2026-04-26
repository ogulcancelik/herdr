//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

pub(crate) mod actions;
mod api;
mod api_helpers;
mod config_io;
mod creation;
mod ids;
mod input;
mod runtime;
mod session;
pub mod state;
mod theme_sync;

use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const HEADLESS_ANIMATION_INTERVAL: Duration = Duration::from_millis(128);
pub(crate) const HEADLESS_ANIMATION_TICK_STEP: u32 = 8;
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GIT_REMOTE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
const AUTO_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);
const SIDEBAR_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);

use crossterm::terminal;
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, Notify};
use tracing::info;

use crate::config::Config;
use crate::events::AppEvent;

pub use state::{AppState, Mode, ToastKind, ViewState};

/// Full application: AppState + runtime concerns (event channels, async I/O).
#[derive(Debug, Clone, Copy)]
pub(crate) struct OverlayPaneState {
    ws_idx: usize,
    tab_idx: usize,
    previous_focus: crate::layout::PaneId,
    previous_zoomed: bool,
}

pub struct App {
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub(crate) event_rx: mpsc::Receiver<AppEvent>,
    pub(crate) api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
    pub(crate) event_hub: crate::api::EventHub,
    pub(crate) last_focus: Option<(usize, crate::layout::PaneId)>,
    pub(crate) no_session: bool,
    pub(crate) input_rx: Option<mpsc::Receiver<crate::raw_input::RawInputEvent>>,
    pub(crate) last_terminal_size: Option<(u16, u16)>,
    pub(crate) config_diagnostic_deadline: Option<Instant>,
    pub(crate) toast_deadline: Option<Instant>,
    pub(crate) last_git_remote_status_refresh: Instant,
    pub(crate) last_sidebar_divider_click: Option<Instant>,
    pub(crate) next_resize_poll: Instant,
    pub(crate) next_animation_tick: Option<Instant>,
    pub(crate) next_auto_update_check: Option<Instant>,
    pub(crate) session_save_deadline: Option<Instant>,
    pub(crate) last_render_at: Option<Instant>,
    pub(crate) suppressed_repeat_keys:
        HashSet<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    pub render_notify: Arc<Notify>,
    pub render_dirty: Arc<AtomicBool>,
    pub(crate) overlay_panes: HashMap<crate::layout::PaneId, OverlayPaneState>,
}

pub(crate) enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(crate::api::ApiRequestMessage),
    RawInput(crate::raw_input::RawInputEvent),
    InputClosed,
    RenderRequested,
}

async fn recv_raw_input_or_pending(
    input_rx: Option<&mut mpsc::Receiver<crate::raw_input::RawInputEvent>>,
) -> Option<crate::raw_input::RawInputEvent> {
    match input_rx {
        Some(rx) => rx.recv().await,
        None => pending().await,
    }
}

async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => pending().await,
    }
}

fn repeat_key_identity(
    key: &crate::input::TerminalKey,
) -> (crossterm::event::KeyCode, crossterm::event::KeyModifiers) {
    (key.code, key.modifiers)
}

fn auto_updates_enabled(no_session: bool) -> bool {
    !no_session && !cfg!(debug_assertions)
}

/// Resolve the palette from config: base theme + optional custom overrides.
fn resolve_palette(config: &crate::config::Config) -> state::Palette {
    // Start with the named theme (default: catppuccin)
    let base_name = config.theme.name.as_deref().unwrap_or("catppuccin");
    let mut palette = state::Palette::from_name(base_name).unwrap_or_else(|| {
        tracing::warn!(
            theme = base_name,
            "unknown theme, falling back to catppuccin"
        );
        state::Palette::catppuccin()
    });

    // Apply custom overrides if present
    if let Some(custom) = &config.theme.custom {
        palette = palette.with_overrides(custom);
    }

    // Legacy: if ui.accent is set and no theme.custom.accent, use it for compat
    if config.ui.accent != "cyan"
        && config
            .theme
            .custom
            .as_ref()
            .and_then(|c| c.accent.as_ref())
            .is_none()
    {
        palette.accent = crate::config::parse_color(&config.ui.accent);
    }

    palette
}

impl App {
    pub fn new(
        config: &Config,
        no_session: bool,
        config_diagnostic: Option<String>,
        startup_release_notes: Option<crate::release_notes::ReleaseNotes>,
        api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
        event_hub: crate::api::EventHub,
    ) -> Self {
        let (prefix_code, prefix_mods) = config.prefix_key();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));

        // Try to restore previous session
        let (workspaces, active, selected, agent_panel_scope, sidebar_width, sidebar_section_split) =
            if no_session {
                (
                    Vec::new(),
                    None,
                    0,
                    state::AgentPanelScope::CurrentWorkspace,
                    config.ui.sidebar_width,
                    0.5_f32,
                )
            } else if let Some(snap) = crate::persist::load() {
                let ws = crate::persist::restore(
                    &snap,
                    24,
                    80,
                    config.advanced.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    event_tx.clone(),
                    render_notify.clone(),
                    render_dirty.clone(),
                );
                if ws.is_empty() {
                    crate::logging::session_restored(0, "empty");
                    (
                        Vec::new(),
                        None,
                        0,
                        snap.agent_panel_scope,
                        snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                        snap.sidebar_section_split.unwrap_or(0.5),
                    )
                } else {
                    crate::logging::session_restored(ws.len(), "ok");
                    let active = snap.active.filter(|&i| i < ws.len());
                    let selected = snap.selected.min(ws.len().saturating_sub(1));
                    (
                        ws,
                        active,
                        selected,
                        snap.agent_panel_scope,
                        snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                        snap.sidebar_section_split.unwrap_or(0.5),
                    )
                }
            } else {
                (
                    Vec::new(),
                    None,
                    0,
                    state::AgentPanelScope::CurrentWorkspace,
                    config.ui.sidebar_width,
                    0.5_f32,
                )
            };

        info!(
            pane_scrollback_limit_bytes = config.advanced.scrollback_limit_bytes,
            "using pane scrollback configuration"
        );

        let latest_release_notes = crate::release_notes::load_latest();
        let update_available = latest_release_notes
            .as_ref()
            .filter(|notes| notes.preview)
            .map(|notes| notes.version.clone());
        let latest_release_notes_available = latest_release_notes.is_some();

        let mode = if config.should_show_onboarding() {
            state::Mode::Onboarding
        } else if startup_release_notes.is_some() {
            state::Mode::ReleaseNotes
        } else if active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        };

        let mut state = AppState {
            workspaces,
            active,
            selected,
            mode,
            should_quit: false,
            quit_detaches: !no_session,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_reload_keybinds: false,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            release_notes: startup_release_notes.map(|notes| state::ReleaseNotesState {
                version: notes.version,
                body: notes.body,
                scroll: 0,
                preview: notes.preview,
            }),
            keybind_help: state::KeybindHelpState { scroll: 0 },
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            view: state::ViewState {
                sidebar_rect: Rect::default(),
                workspace_card_areas: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            workspace_press: None,
            tab_press: None,
            selection: None,
            context_menu: None,
            update_available,
            latest_release_notes_available,
            update_dismissed: false,
            config_diagnostic,
            toast: None,
            prefix_code,
            prefix_mods,
            default_sidebar_width: config.ui.sidebar_width,
            sidebar_width,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            sidebar_section_split,
            agent_panel_scope,
            confirm_close: config.ui.confirm_close,
            pane_scrollback_limit_bytes: config.advanced.scrollback_limit_bytes,
            accent: crate::config::parse_color(&config.ui.accent),
            sound: config.ui.sound.clone(),
            toast_config: config.ui.toast.clone(),
            keybinds: config.keybinds(),
            spinner_tick: 0,
            palette: resolve_palette(&config),
            theme_name: config
                .theme
                .name
                .clone()
                .unwrap_or_else(|| "catppuccin".to_string()),
            settings: state::SettingsState {
                section: state::SettingsSection::Theme,
                list: state::SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
            },
            global_menu: state::MenuListState::new(0),
            host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
            session_dirty: false,
            session_picker_enabled: !no_session,
            active_session: None,
            session_picker: state::SessionPickerState {
                sessions: Vec::new(),
                highlighted: 0,
                scroll: 0,
                creating: false,
                name_input: String::new(),
                pending_delete: None,
                error: None,
            },
            request_open_session_picker: false,
        };

        for ws in &mut state.workspaces {
            ws.refresh_git_ahead_behind();
        }

        // Background auto-update is disabled in monolithic no-session mode
        // and in debug/test builds so local development never mutates the
        // running binary out from under spawned test processes.
        if auto_updates_enabled(no_session) {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }

        let last_focus = state.active.and_then(|idx| {
            state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });

        Self {
            config_diagnostic_deadline: state
                .config_diagnostic
                .as_ref()
                .map(|_| Instant::now() + Duration::from_secs(8)),
            toast_deadline: None,
            state,
            event_tx,
            event_rx,
            last_git_remote_status_refresh: Instant::now(),
            last_sidebar_divider_click: None,
            next_resize_poll: Instant::now() + RESIZE_POLL_INTERVAL,
            next_animation_tick: None,
            next_auto_update_check: auto_updates_enabled(no_session)
                .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL),
            session_save_deadline: None,
            last_render_at: None,
            suppressed_repeat_keys: HashSet::new(),
            api_rx,
            event_hub,
            last_focus,
            no_session,
            input_rx: None,
            last_terminal_size: terminal::size().ok(),
            render_notify,
            render_dirty,
            overlay_panes: HashMap::new(),
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        if self.input_rx.is_none() {
            self.input_rx = Some(crate::raw_input::spawn_input_reader());
        }
        self.query_host_terminal_theme();

        let mut needs_render = true;

        while !self.state.should_quit {
            if self.render_dirty.load(Ordering::Acquire) {
                needs_render = true;
            }

            // Drain internal events first so API reads observe fresh pane state.
            if self.drain_internal_events() {
                needs_render = true;
            }
            if self.drain_api_requests() {
                needs_render = true;
            }

            self.sync_focus_events();
            self.sync_session_save_schedule();

            let now = Instant::now();
            if self.handle_scheduled_tasks(now) {
                needs_render = true;
            }

            if self.state.request_complete_onboarding {
                self.state.request_complete_onboarding = false;
                self.open_settings_from_onboarding();
                needs_render = true;
            }

            if self.state.request_new_workspace {
                self.state.request_new_workspace = false;
                self.create_workspace();
                needs_render = true;
            }

            if self.state.request_new_tab {
                self.state.request_new_tab = false;
                self.create_tab();
                needs_render = true;
            }

            if self.state.request_reload_keybinds {
                self.state.request_reload_keybinds = false;
                self.reload_keybinds();
                needs_render = true;
            }

            if self.state.request_open_session_picker {
                self.state.request_open_session_picker = false;
                self.open_session_picker();
                needs_render = true;
            }

            self.ensure_session_picker_populated();

            let now = Instant::now();
            self.sync_animation_timer(now);

            if needs_render && self.can_render_now(now) {
                self.render_dirty.swap(false, Ordering::AcqRel);
                terminal.draw(|frame| {
                    crate::ui::compute_view(&mut self.state, frame.area());
                    crate::ui::render(&self.state, frame);
                })?;
                self.last_render_at = Some(now);
                needs_render = false;
                continue;
            }

            let next_deadline = self.next_loop_deadline(now, needs_render);
            let event = {
                let input_rx = self.input_rx.as_mut();
                tokio::select! {
                    maybe_api = self.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(msg),
                        None => LoopEvent::Timer,
                    },
                    maybe_ev = self.event_rx.recv() => match maybe_ev {
                        Some(ev) => LoopEvent::Internal(ev),
                        None => LoopEvent::Timer,
                    },
                    maybe_input = recv_raw_input_or_pending(input_rx) => match maybe_input {
                        Some(input) => LoopEvent::RawInput(input),
                        None => LoopEvent::InputClosed,
                    },
                    _ = sleep_until_or_pending(next_deadline) => LoopEvent::Timer,
                    _ = self.render_notify.notified() => LoopEvent::RenderRequested,
                }
            };

            match event {
                LoopEvent::Timer => {}
                LoopEvent::Internal(ev) => {
                    self.handle_internal_event(ev);
                    needs_render = true;
                }
                LoopEvent::Api(msg) => {
                    if self.handle_api_request_message(msg) {
                        needs_render = true;
                    }
                }
                LoopEvent::RawInput(input) => {
                    if self.handle_raw_input_batch(input).await {
                        needs_render = true;
                    }
                }
                LoopEvent::InputClosed => {
                    self.input_rx = None;
                }
                LoopEvent::RenderRequested => {
                    if self.render_dirty.load(Ordering::Acquire) {
                        needs_render = true;
                    }
                }
            }
        }

        // Save session on exit (skip in --no-session mode)
        if !self.no_session {
            self.save_session_now();
        }

        Ok(())
    }

    pub(crate) fn dismiss_release_notes(&mut self) {
        let preview = self
            .state
            .release_notes
            .as_ref()
            .is_some_and(|notes| notes.preview);

        self.state.release_notes = None;
        if !preview {
            if let Err(err) = crate::release_notes::mark_current_version_seen() {
                self.state.config_diagnostic =
                    Some(format!("failed to update release notes status: {err}"));
                self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
            }
        }

        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    pub(crate) fn scroll_release_notes(&mut self, delta: i16) {
        let max_scroll = self.state.release_notes_max_scroll();
        if let Some(notes) = &mut self.state.release_notes {
            notes.scroll = if delta.is_negative() {
                notes.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                notes.scroll.saturating_add(delta as u16)
            }
            .min(max_scroll);
        }
    }

    pub(crate) fn open_settings_from_onboarding(&mut self) {
        self.mark_onboarding_complete();
        crate::app::input::open_settings(&mut self.state);
    }

    pub(crate) fn reload_keybinds(&mut self) {
        let previous_toast = self.state.toast.clone();
        match crate::config::load_live_keybinds() {
            Ok(live) => {
                self.state.prefix_code = live.prefix.0;
                self.state.prefix_mods = live.prefix.1;
                self.state.keybinds = live.keybinds;
                self.state.config_diagnostic = None;
                self.config_diagnostic_deadline = None;
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::UpdateInstalled,
                    title: "reloaded keybinds".to_string(),
                    context: "using config.toml".to_string(),
                });
            }
            Err(diagnostics) => {
                let mut message = diagnostics.join("; ");
                if !message.contains("keeping current keybinds") {
                    message.push_str("; keeping current keybinds");
                }
                self.state.toast = None;
                self.state.config_diagnostic = Some(message);
                self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(8));
            }
        }
        self.sync_toast_deadline(previous_toast);
    }
}

// ---------------------------------------------------------------------------
// Input routing for headless server mode
// ---------------------------------------------------------------------------

impl App {
    /// Routes raw input bytes from a client through the existing input pipeline.
    ///
    /// The input bytes are parsed into `RawInputEvent`s and then processed.
    /// In terminal mode, keys are routed through the same semantic
    /// key-handling path as monolithic herdr so they are re-encoded for the
    /// focused pane's negotiated keyboard protocol instead of passing host
    /// terminal escape sequences through unchanged.
    #[cfg(test)]
    pub(crate) fn route_client_input(&mut self, data: Vec<u8>) {
        let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
        self.route_client_events(events, true);
    }

    pub(crate) fn route_client_events(
        &mut self,
        events: Vec<crate::raw_input::RawInputEvent>,
        apply_host_terminal_theme: bool,
    ) {
        for event in events {
            match event {
                crate::raw_input::RawInputEvent::Key(key) => {
                    let key_id = repeat_key_identity(&key);
                    match key.kind {
                        crossterm::event::KeyEventKind::Press => {
                            if self.state.mode == Mode::Terminal {
                                self.suppressed_repeat_keys.remove(&key_id);
                                self.handle_terminal_key_headless(key);
                            } else {
                                self.suppressed_repeat_keys.insert(key_id);
                                self.handle_non_terminal_key(key);
                            }
                        }
                        crossterm::event::KeyEventKind::Repeat => {
                            if self.state.mode == Mode::Terminal
                                && !self.suppressed_repeat_keys.contains(&key_id)
                            {
                                self.handle_terminal_key_headless(key);
                            }
                            // Repeats in non-terminal modes are ignored
                            // (same as monolithic behavior).
                        }
                        crossterm::event::KeyEventKind::Release => {
                            self.suppressed_repeat_keys.remove(&key_id);
                        }
                    }
                }
                crate::raw_input::RawInputEvent::Mouse(mouse) => {
                    self.handle_mouse_event_headless(mouse);
                }
                crate::raw_input::RawInputEvent::Paste(text) => {
                    if self.state.mode == Mode::Terminal {
                        if let Some(ws_idx) = self.state.active {
                            if let Some(ws) = self.state.workspaces.get(ws_idx) {
                                if let Some(focused) = ws.focused_pane_id() {
                                    if let Some(runtime) = ws.runtimes.get(&focused) {
                                        let _ = runtime.try_send_bytes(bytes::Bytes::from(
                                            if runtime
                                                .input_state()
                                                .map(|s| s.bracketed_paste)
                                                .unwrap_or(false)
                                            {
                                                format!("\x1b[200~{text}\x1b[201~")
                                            } else {
                                                text
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    if apply_host_terminal_theme {
                        self.update_host_terminal_theme(kind, color);
                    }
                }
                crate::raw_input::RawInputEvent::Unsupported => {}
            }
        }
    }

    /// Handles a key event in non-terminal mode for the headless server.
    ///
    /// Uses the standalone handler functions that work on `&mut AppState`
    /// since the server doesn't have the async context of the monolithic App.
    fn handle_non_terminal_key(&mut self, key: crate::input::TerminalKey) {
        let key_event = key.as_key_event();
        match self.state.mode {
            Mode::Navigate => {
                input::handle_navigate_key(&mut self.state, key_event);
            }
            Mode::RenameWorkspace | Mode::RenameTab => {
                input::handle_rename_key(&mut self.state, key_event);
            }
            Mode::Resize => {
                input::handle_resize_key(&mut self.state, key_event);
            }
            Mode::ConfirmClose => {
                input::handle_confirm_close_key(&mut self.state, key_event);
            }
            Mode::ContextMenu => {
                input::handle_context_menu_key(&mut self.state, key_event);
            }
            Mode::KeybindHelp => {
                input::handle_keybind_help_key(&mut self.state, key_event);
            }
            Mode::GlobalMenu => {
                input::handle_global_menu_key(&mut self.state, key_event);
            }
            Mode::Onboarding => {
                self.handle_onboarding_key(key_event);
            }
            Mode::ReleaseNotes => {
                self.handle_release_notes_key(key_event);
            }
            Mode::Settings => {
                self.handle_settings_key(key_event);
            }
            Mode::SessionPicker => {
                if let Some(action) = input::update_session_picker_state(&mut self.state, key_event)
                {
                    self.execute_session_picker_action(action);
                }
            }
            Mode::Terminal => {
                // Should not be called in terminal mode.
            }
        }
    }

    /// Handles a mouse event for the headless server.
    ///
    /// Delegates to the same mouse handling logic used in the monolithic
    /// mode (hit-testing against the rendered UI), which works because
    /// the server's AppState maintains view geometry from virtual rendering.
    fn handle_mouse_event_headless(&mut self, mouse: crossterm::event::MouseEvent) {
        self.handle_mouse(mouse);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::pane::PaneRuntime;
    use crate::workspace::Workspace;
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    use std::sync::{Mutex, OnceLock};

    fn raw_key(
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
    ) -> crate::raw_input::RawInputEvent {
        crate::raw_input::RawInputEvent::Key(
            crate::input::TerminalKey::new(code, modifiers).with_kind(kind),
        )
    }

    fn release_notes_state() -> state::ReleaseNotesState {
        state::ReleaseNotesState {
            version: "0.1.0".into(),
            body: "notes".into(),
            scroll: 0,
            preview: true,
        }
    }

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            true,
            None,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn config_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let unique = format!(
            "herdr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("config.toml")
    }

    #[test]
    fn startup_restores_preview_update_available_from_saved_notes() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("startup-preview-update-available");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        // Use a bogus far-future version so preview=true regardless of current binary version.
        crate::release_notes::save_pending("99.99.99", "### Changed\n- One").unwrap();

        let app = test_app();

        assert_eq!(app.state.update_available.as_deref(), Some("99.99.99"));
        assert!(app.state.latest_release_notes_available);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_does_not_restore_update_available_from_older_saved_notes() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("startup-stale-update-notes");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        crate::release_notes::save_pending("0.4.9", "### Changed\n- One").unwrap();

        let app = test_app();

        assert_eq!(app.state.update_available, None);
        assert!(app.state.latest_release_notes_available);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_keybinds_updates_live_state() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-keybinds-success");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nnew_workspace = \"g\"\nprefix = \"ctrl+a\"\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.reload_keybinds();

        assert_eq!(app.state.prefix_code, KeyCode::Char('a'));
        assert_eq!(app.state.prefix_mods, KeyModifiers::CONTROL);
        assert_eq!(
            app.state.keybinds.new_workspace,
            (KeyCode::Char('g'), KeyModifiers::empty())
        );
        assert!(app.state.config_diagnostic.is_none());
        let toast = app.state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, crate::app::state::ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "reloaded keybinds");
        assert_eq!(toast.context, "using config.toml");

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_keybinds_keeps_current_state_on_invalid_binding() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-keybinds-invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[keys]\nnew_workspace = \"wat\"\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_prefix = (app.state.prefix_code, app.state.prefix_mods);
        let original_keybinds = app.state.keybinds.new_workspace;
        app.reload_keybinds();

        assert_eq!(
            (app.state.prefix_code, app.state.prefix_mods),
            original_prefix
        );
        assert_eq!(app.state.keybinds.new_workspace, original_keybinds);
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| {
                message.contains("keys.new_workspace")
                    && message.contains("keeping current keybinds")
            }));
        assert!(app.state.toast.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn raw_input_waits_when_reader_is_gone() {
        let result =
            tokio::time::timeout(Duration::from_millis(20), recv_raw_input_or_pending(None)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn terminal_mode_handles_repeat_key_events() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Backspace,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;

        assert!(handled);
    }

    #[tokio::test]
    async fn repeat_key_events_are_ignored_outside_terminal_mode() {
        let mut app = test_app();
        app.state.mode = Mode::ReleaseNotes;
        app.state.release_notes = Some(release_notes_state());

        let handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;

        assert!(!handled);
        assert_eq!(app.state.mode, Mode::ReleaseNotes);
        assert!(app.state.release_notes.is_some());
    }

    #[tokio::test]
    async fn modal_press_does_not_leak_repeat_into_terminal_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::ReleaseNotes;
        app.state.release_notes = Some(release_notes_state());

        let press_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Press,
            ))
            .await;
        let repeat_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Repeat,
            ))
            .await;
        let release_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Release,
            ))
            .await;
        let next_press_handled = app
            .handle_raw_input_event(raw_key(
                KeyCode::Enter,
                KeyModifiers::empty(),
                KeyEventKind::Press,
            ))
            .await;

        assert!(press_handled);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!repeat_handled);
        assert!(!release_handled);
        assert!(next_press_handled);
    }

    #[test]
    fn read_only_api_requests_do_not_force_rerender() {
        let read_only = crate::api::schema::Request {
            id: "req_1".into(),
            method: crate::api::schema::Method::WorkspaceList(
                crate::api::schema::EmptyParams::default(),
            ),
        };
        let mutating = crate::api::schema::Request {
            id: "req_2".into(),
            method: crate::api::schema::Method::WorkspaceFocus(
                crate::api::schema::WorkspaceTarget {
                    workspace_id: "w_1".into(),
                },
            ),
        };

        assert!(!crate::api::request_changes_ui(&read_only));
        assert!(crate::api::request_changes_ui(&mutating));
    }

    #[test]
    fn workspace_create_response_includes_initial_tab_and_root_pane() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("api-root-pane")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace,
            tab,
            root_pane,
        } = app.workspace_created_result(0).unwrap()
        else {
            panic!("expected workspace_created response");
        };

        assert_eq!(workspace.label, "api-root-pane");
        assert_eq!(tab.workspace_id, workspace.workspace_id);
        assert_eq!(root_pane.workspace_id, workspace.workspace_id);
        assert_eq!(root_pane.tab_id, tab.tab_id);
    }

    #[test]
    fn tab_create_response_includes_root_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-tab-root-pane");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let crate::api::schema::ResponseResult::TabCreated { tab, root_pane } =
            app.tab_created_result(0, 1).unwrap()
        else {
            panic!("expected tab_created response");
        };

        assert_eq!(tab.workspace_id, root_pane.workspace_id);
        assert_eq!(root_pane.tab_id, tab.tab_id);
        assert_eq!(tab.pane_count, 1);
    }

    #[test]
    fn workspace_creation_in_navigate_mode_uses_selected_workspace_seed_cwd() {
        let mut app = test_app();
        let mut first = Workspace::test_new("herdr");
        let first_root = first.tabs[0].root_pane;
        first.tabs[0]
            .pane_cwds
            .insert(first_root, std::path::PathBuf::from("/tmp/herdr"));
        let mut second = Workspace::test_new("pion");
        let second_root = second.tabs[0].root_pane;
        second.tabs[0]
            .pane_cwds
            .insert(second_root, std::path::PathBuf::from("/tmp/pion"));

        app.state.workspaces = vec![first, second];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::Navigate;

        let ws_idx = app.workspace_creation_source().unwrap();
        let seed_cwd = app.seed_cwd_from_workspace(ws_idx).unwrap();

        assert_eq!(ws_idx, 1);
        assert_eq!(seed_cwd, std::path::PathBuf::from("/tmp/pion"));
    }

    #[test]
    fn server_stop_request_sets_should_quit_flag() {
        let mut app = test_app();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_server_stop".into(),
            method: crate::api::schema::Method::ServerStop(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert!(app.state.should_quit);
    }

    #[test]
    fn pane_close_request_closes_only_the_target_tab_when_other_tabs_exist() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-close");
        let second_tab = workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(second_tab);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane = app.state.workspaces[0].tabs[second_tab].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "api-pane-close");
    }

    #[test]
    fn pane_close_request_closes_workspace_when_it_removes_the_last_pane() {
        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-close-last");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane = app.state.workspaces[0].tabs[0].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close_last".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn session_dirty_flag_schedules_debounced_save() {
        let mut app = test_app();
        app.no_session = false;
        app.state.session_dirty = true;

        app.sync_session_save_schedule();

        assert!(!app.state.session_dirty);
        assert!(app.session_save_deadline.is_some());
    }

    #[test]
    fn next_loop_deadline_includes_session_save_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.session_save_deadline = Some(now + Duration::from_secs(2));
        app.next_resize_poll = now + Duration::from_secs(5);
        app.next_auto_update_check = Some(now + Duration::from_secs(6));

        assert_eq!(
            app.next_loop_deadline(now, false),
            app.session_save_deadline
        );
    }

    #[test]
    fn headless_next_loop_deadline_ignores_resize_poll() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now + Duration::from_millis(100);
        app.session_save_deadline = Some(now + Duration::from_secs(2));
        app.next_auto_update_check = Some(now + Duration::from_secs(6));

        assert_eq!(
            app.next_headless_loop_deadline(now, false),
            app.session_save_deadline
        );
    }

    #[test]
    fn headless_next_loop_deadline_returns_none_when_resize_poll_is_only_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now - Duration::from_millis(1);
        app.config_diagnostic_deadline = None;
        app.toast_deadline = None;
        app.next_animation_tick = None;
        app.next_auto_update_check = None;
        app.session_save_deadline = None;
        app.state.workspaces.clear();

        assert_eq!(app.next_headless_loop_deadline(now, false), None);
    }

    #[test]
    fn due_session_save_deadline_is_cleared() {
        let mut app = test_app();
        app.session_save_deadline = Some(Instant::now() - Duration::from_secs(1));

        app.handle_scheduled_tasks(Instant::now());

        assert!(app.session_save_deadline.is_none());
    }

    #[tokio::test]
    async fn full_internal_event_queue_eventually_applies_working_to_idle_transition() {
        let mut app = test_app();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
        });
        assert_eq!(
            app.state.workspaces[0].pane_state(pane_id).unwrap().state,
            AgentState::Working
        );

        for i in 0..64 {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("9.9.{i}"),
                })
                .unwrap();
        }

        let tx = app.event_tx.clone();
        let send = tx.send(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });
        tokio::pin!(send);

        let blocked =
            tokio::time::timeout(Duration::from_millis(20), async { (&mut send).await }).await;
        assert!(
            blocked.is_err(),
            "state change sender should wait for queue space instead of failing"
        );

        app.drain_internal_events();

        tokio::time::timeout(Duration::from_millis(50), async { (&mut send).await })
            .await
            .expect("state change should enqueue once queue space is available")
            .expect("app event receiver should still be alive");

        app.drain_internal_events();

        assert_eq!(
            app.state.workspaces[0].pane_state(pane_id).unwrap().state,
            AgentState::Idle,
            "Working→Idle should still apply after temporary queue pressure"
        );
    }

    #[test]
    fn route_client_input_dispatches_navigate_mode_keybinds() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;

        // Start in navigate mode.
        app.state.mode = Mode::Navigate;

        // Send Ctrl+B then Esc (prefix → leave navigate mode).
        // Ctrl+B is 0x02 in raw terminal input.
        // After entering navigate mode and pressing Esc, we should leave navigate mode.
        let esc_bytes = vec![0x1b]; // Esc
        app.route_client_input(esc_bytes);
        // Esc in navigate mode should leave navigate mode.
        assert_eq!(
            app.state.mode,
            Mode::Terminal,
            "Esc should leave navigate mode and return to Terminal mode"
        );
    }

    #[test]
    fn route_client_input_q_detaches_in_persistence_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.quit_detaches = true;

        // Start in navigate mode.
        app.state.mode = Mode::Navigate;
        assert!(!app.state.detach_requested);

        let q_bytes = b"q".to_vec();
        app.route_client_input(q_bytes);

        assert!(
            app.state.detach_requested,
            "q should detach in persistence mode"
        );
        assert_eq!(
            app.state.mode,
            Mode::Terminal,
            "q should leave navigate mode"
        );
    }

    #[test]
    fn route_client_input_prefix_then_q_detaches_in_persistence_mode() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.quit_detaches = true;

        // Start in terminal mode (default after workspace creation).
        app.state.mode = Mode::Terminal;
        assert!(!app.state.detach_requested);

        // Send Ctrl+B (prefix key, raw byte 0x02).
        let prefix_bytes = vec![0x02];
        app.route_client_input(prefix_bytes);

        assert_eq!(
            app.state.mode,
            Mode::Navigate,
            "prefix key should enter navigate mode"
        );
        assert!(
            !app.state.detach_requested,
            "prefix key should not set detach flag"
        );

        let q_bytes = b"q".to_vec();
        app.route_client_input(q_bytes);

        assert!(
            app.state.detach_requested,
            "q should detach in persistence mode"
        );
        assert_eq!(
            app.state.mode,
            Mode::Terminal,
            "q should leave navigate mode"
        );
    }

    #[tokio::test]
    async fn route_client_input_reencodes_terminal_keys_for_focused_pane_protocol() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        // Ghostty/kitty-style Ctrl-C should be normalized back to the pane's
        // negotiated encoding instead of being forwarded verbatim.
        app.route_client_input(b"\x1b[99;5u".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from(vec![3]));
    }

    #[tokio::test]
    async fn route_client_input_splits_multi_event_payloads_before_forwarding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.route_client_input(b"ab".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"a"));
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from_static(b"b"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn route_client_input_handles_mouse_events() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;

        // Send a mouse scroll-up event via SGR encoding.
        let mouse_bytes = b"\x1b[<64;10;5M".to_vec();
        // This should not panic even though mouse handling is simplified
        // in headless mode.
        app.route_client_input(mouse_bytes);
        // No assertions on specific behavior — just no panic.
    }

    #[test]
    fn route_client_input_advances_onboarding_modal() {
        let mut app = test_app();
        app.state.mode = Mode::Onboarding;

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.mode, Mode::Settings);
    }

    #[test]
    fn route_client_input_closes_release_notes_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::ReleaseNotes;
        app.state.release_notes = Some(release_notes_state());

        app.route_client_input(b"\x1b".to_vec());

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.release_notes.is_none());
    }

    #[test]
    fn route_client_input_closes_settings_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Settings;
        app.state.settings.original_theme = Some(app.state.theme_name.clone());
        app.state.settings.original_palette = Some(app.state.palette.clone());

        app.route_client_input(b"\x1b".to_vec());

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn route_client_input_updates_host_terminal_theme_from_osc_response() {
        let mut app = test_app();

        app.route_client_input(b"\x1b]11;#123456\x07".to_vec());

        assert_eq!(
            app.state.host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
    }

    #[test]
    fn parse_raw_input_bytes_with_ranges_tracks_offsets() {
        // Verify that the range-aware parser correctly tracks byte offsets
        // for events within a multi-event input buffer.
        let input = b"\x1b[Aa".to_vec(); // Up arrow + 'a'
        let events = crate::raw_input::parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(events.len(), 2, "should parse Up arrow and 'a'");
        // Up arrow: \x1b[A = 3 bytes starting at offset 0
        assert_eq!(events[0].start, 0);
        assert_eq!(events[0].len, 3);
        // 'a': 1 byte starting at offset 3
        assert_eq!(events[1].start, 3);
        assert_eq!(events[1].len, 1);

        // Verify the raw bytes for each event are correct.
        assert_eq!(
            &input[events[0].start..events[0].start + events[0].len],
            b"\x1b[A"
        );
        assert_eq!(
            &input[events[1].start..events[1].start + events[1].len],
            b"a"
        );
    }
}
