//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

pub(crate) mod actions;
mod input;
pub mod state;

use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GIT_REMOTE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
const AUTO_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);
const SIDEBAR_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);

use crossterm::terminal;
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, Notify};
use tracing::{error, info};

use crate::config::Config;
use crate::events::AppEvent;
use crate::workspace::Workspace;

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

fn api_request_changes_ui(request: &crate::api::schema::Request) -> bool {
    use crate::api::schema::Method;

    matches!(
        &request.method,
        Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceClose(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabClose(_)
            | Method::PaneSplit(_)
            | Method::PaneReportAgent(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
    )
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
                    event_tx.clone(),
                    render_notify.clone(),
                    render_dirty.clone(),
                );
                if ws.is_empty() {
                    info!("session file found but no workspaces restored");
                    (
                        Vec::new(),
                        None,
                        0,
                        snap.agent_panel_scope,
                        snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                        snap.sidebar_section_split.unwrap_or(0.5),
                    )
                } else {
                    info!(count = ws.len(), "session restored");
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
            creating_new_tab: false,
            requested_new_tab_name: None,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            onboarding_step: 0,
            onboarding_list: state::SelectionListState::new(1),
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

    fn schedule_session_save(&mut self) {
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
                self.complete_onboarding();
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

    pub(crate) fn drain_api_requests(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.api_rx.try_recv() {
            changed |= self.handle_api_request_message(msg);
        }
        changed
    }

    fn handle_api_request_message(&mut self, msg: crate::api::ApiRequestMessage) -> bool {
        let changed = api_request_changes_ui(&msg.request);
        let response = self.handle_api_request(msg.request);
        let _ = msg.respond_to.send(response);
        changed
    }

    async fn handle_raw_input_batch(&mut self, first: crate::raw_input::RawInputEvent) -> bool {
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

    async fn handle_raw_input_event(&mut self, event: crate::raw_input::RawInputEvent) -> bool {
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
            crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                self.update_host_terminal_theme(kind, color)
            }
            crate::raw_input::RawInputEvent::Unsupported => false,
        }
    }

    fn query_host_terminal_theme(&self) {
        use std::io::Write;

        let _ = std::io::stdout()
            .write_all(crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes());
        let _ = std::io::stdout().flush();
    }

    fn update_host_terminal_theme(
        &mut self,
        kind: crate::terminal_theme::DefaultColorKind,
        color: crate::terminal_theme::RgbColor,
    ) -> bool {
        let next_theme = self.state.host_terminal_theme.with_color(kind, color);
        if next_theme == self.state.host_terminal_theme {
            return false;
        }
        self.state.host_terminal_theme = next_theme;
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
        if self.agent_panel_has_animation() {
            self.next_animation_tick
                .get_or_insert(now + ANIMATION_INTERVAL);
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
        let render_deadline = if needs_render {
            self.last_render_at
                .map(|last_render_at| last_render_at + MIN_RENDER_INTERVAL)
                .filter(|deadline| *deadline > now)
        } else {
            None
        };

        [
            Some(self.next_resize_poll),
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

    pub(crate) fn handle_internal_event(&mut self, ev: AppEvent) {
        if let AppEvent::ClipboardWrite { content } = ev {
            crate::selection::write_osc52_bytes(&content);
            return;
        }

        let overlay_state = if let AppEvent::PaneDied { pane_id } = &ev {
            self.overlay_panes.remove(pane_id)
        } else {
            None
        };

        if let AppEvent::PaneDied { pane_id } = &ev {
            if let Some((ws_idx, _)) = self.find_pane(*pane_id) {
                if let Some(public_pane_id) = self.public_pane_id(ws_idx, *pane_id) {
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneExited,
                        data: crate::api::schema::EventData::PaneExited {
                            pane_id: public_pane_id,
                            workspace_id: self.public_workspace_id(ws_idx),
                        },
                    });
                }
            }
        }

        let released_agent = if let AppEvent::HookAgentReleased {
            pane_id,
            known_agent,
            ..
        } = &ev
        {
            known_agent.map(|agent| (*pane_id, agent))
        } else {
            None
        };

        let previous_toast = self.state.toast.clone();
        let pane_updates = self.state.handle_app_event(ev);
        for update in &pane_updates {
            self.emit_pane_state_update(update);
        }
        if let Some((pane_id, agent)) = released_agent {
            if pane_updates.iter().any(|update| update.pane_id == pane_id) {
                if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                    if let Some(runtime) = self.state.workspaces[ws_idx].runtimes.get(&pane_id) {
                        runtime.begin_graceful_release(agent);
                    }
                }
            }
        }
        if let Some(overlay) = overlay_state {
            self.restore_overlay_after_exit(overlay);
        }
        self.sync_toast_deadline(previous_toast);
    }

    fn restore_overlay_after_exit(&mut self, overlay: OverlayPaneState) {
        let Some(ws) = self.state.workspaces.get_mut(overlay.ws_idx) else {
            return;
        };
        if overlay.tab_idx >= ws.tabs.len() {
            return;
        }

        ws.active_tab = overlay.tab_idx;
        let tab = &mut ws.tabs[overlay.tab_idx];
        if tab.panes.contains_key(&overlay.previous_focus) {
            tab.layout.focus_pane(overlay.previous_focus);
        }
        tab.zoomed = overlay.previous_zoomed;

        if self.state.active == Some(overlay.ws_idx) {
            self.state.mode = Mode::Terminal;
        }
    }

    fn emit_pane_state_update(&self, update: &crate::app::actions::PaneStateUpdate) {
        let Some(pane_id) = self.public_pane_id(update.ws_idx, update.pane_id) else {
            return;
        };
        let workspace_id = self.public_workspace_id(update.ws_idx);

        if update.previous_agent_label != update.agent_label {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentDetected,
                data: crate::api::schema::EventData::PaneAgentDetected {
                    pane_id: pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                    agent: update.agent_label.clone(),
                },
            });
        }

        if update.previous_state != update.state {
            let agent_status = self
                .state
                .workspaces
                .get(update.ws_idx)
                .and_then(|ws| ws.pane_state(update.pane_id))
                .map(|pane| pane_agent_status(pane.state, pane.seen))
                .unwrap_or_else(|| pane_agent_status(update.state, true));
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id,
                    workspace_id,
                    agent_status,
                },
            });
        }
    }

    fn sync_toast_deadline(
        &mut self,
        previous_toast: Option<crate::app::state::ToastNotification>,
    ) {
        if self.state.toast != previous_toast {
            self.toast_deadline = self.state.toast.as_ref().map(|toast| {
                let duration = match toast.kind {
                    ToastKind::NeedsAttention => Duration::from_secs(8),
                    ToastKind::Finished => Duration::from_secs(5),
                    ToastKind::UpdateInstalled => Duration::from_secs(3),
                };
                Instant::now() + duration
            });
        }
    }

    fn emit_event(&self, event: crate::api::schema::EventEnvelope) {
        self.event_hub.push(event);
    }

    pub(crate) fn sync_focus_events(&mut self) {
        let current_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        if current_focus == self.last_focus {
            return;
        }

        if let Some((ws_idx, pane_id)) = self.last_focus {
            self.send_pane_focus_event(ws_idx, pane_id, crate::ghostty::FocusEvent::Lost);
        }
        if let Some((ws_idx, pane_id)) = current_focus {
            self.send_pane_focus_event(ws_idx, pane_id, crate::ghostty::FocusEvent::Gained);
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorkspaceFocused,
                data: crate::api::schema::EventData::WorkspaceFocused {
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
            if let Some(tab_id) =
                self.public_tab_id(ws_idx, self.state.workspaces[ws_idx].active_tab)
            {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabFocused,
                    data: crate::api::schema::EventData::TabFocused {
                        tab_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
            }
            if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneFocused,
                    data: crate::api::schema::EventData::PaneFocused {
                        pane_id: public_pane_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
            }
        }

        self.last_focus = current_focus;
    }

    fn send_pane_focus_event(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        event: crate::ghostty::FocusEvent,
    ) {
        let Some(runtime) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.runtime(pane_id))
        else {
            return;
        };
        runtime.try_send_focus_event(event);
    }

    fn find_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<(usize, &crate::pane::PaneState)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| ws.pane_state(pane_id).map(|pane| (ws_idx, pane)))
    }

    fn public_workspace_id(&self, ws_idx: usize) -> String {
        self.state.workspaces[ws_idx].id.clone()
    }

    fn public_tab_id(&self, ws_idx: usize, tab_idx: usize) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        ws.tabs.get(tab_idx)?;
        Some(format!("{}:{}", ws.id, tab_idx + 1))
    }

    fn public_pane_id(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        Some(format!("{}-{pane_number}", ws.id))
    }

    fn parse_workspace_id(&self, id: &str) -> Option<usize> {
        self.state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == id)
            .or_else(|| id.strip_prefix("w_")?.parse::<usize>().ok()?.checked_sub(1))
            .or_else(|| id.parse::<usize>().ok()?.checked_sub(1))
    }

    fn parse_tab_id(&self, id: &str) -> Option<(usize, usize)> {
        if let Some(rest) = id.strip_prefix("t_") {
            let (ws_raw, tab_raw) = rest.rsplit_once('_')?;
            let ws_idx = self.parse_workspace_id(ws_raw)?;
            let tab_idx = tab_raw.parse::<usize>().ok()?.checked_sub(1)?;
            self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
            return Some((ws_idx, tab_idx));
        }

        let (ws_raw, tab_raw) = id.rsplit_once(':')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let tab_idx = tab_raw.parse::<usize>().ok()?.checked_sub(1)?;
        self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        Some((ws_idx, tab_idx))
    }

    fn parse_pane_id(&self, id: &str) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(rest) = id.strip_prefix("p_") {
            if let Some((ws_raw, pane_raw)) = rest.rsplit_once('_') {
                let ws_idx = self.parse_workspace_id(ws_raw)?;
                let pane_id = crate::layout::PaneId::from_raw(pane_raw.parse::<u32>().ok()?);
                return Some((ws_idx, pane_id));
            }

            let pane_id = crate::layout::PaneId::from_raw(rest.parse::<u32>().ok()?);
            return self.find_pane(pane_id).map(|(ws_idx, _)| (ws_idx, pane_id));
        }

        let (ws_raw, pane_number_raw) = id.rsplit_once('-')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let pane_number = pane_number_raw.parse::<usize>().ok()?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws
            .public_pane_numbers
            .iter()
            .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
        Some((ws_idx, pane_id))
    }

    pub(crate) fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_internal_events();
        use bytes::Bytes;

        use crate::api::schema::{
            ErrorBody, ErrorResponse, IntegrationInstallResult, IntegrationTarget,
            IntegrationUninstallResult, Method, PaneListParams, PaneReadResult, ReadSource,
            ResponseResult, SuccessResponse, TabListParams,
        };

        let response = match request.method {
            Method::ServerStop(_) => {
                self.state.should_quit = true;
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::WorkspaceList(_) => SuccessResponse {
                id: request.id,
                result: ResponseResult::WorkspaceList {
                    workspaces: self
                        .state
                        .workspaces
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| self.workspace_info(idx))
                        .collect(),
                },
            },
            Method::WorkspaceGet(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                let Some(_) = self.state.workspaces.get(index) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceCreate(params) => {
                let cwd = params
                    .cwd
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("/"));
                match self.create_workspace_with_options(cwd, params.focus) {
                    Ok(index) => {
                        if let Some(label) = params.label {
                            if let Some(workspace) = self.state.workspaces.get_mut(index) {
                                workspace.set_custom_name(label);
                            }
                        }
                        let workspace = self.workspace_info(index);
                        let tab = self
                            .tab_info(index, 0)
                            .expect("new workspace should have an initial tab");
                        let root_pane = self
                            .root_pane_info(index, 0)
                            .expect("new workspace should have an initial root pane");
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::WorkspaceCreated,
                            data: crate::api::schema::EventData::WorkspaceCreated {
                                workspace: workspace.clone(),
                            },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::TabCreated,
                            data: crate::api::schema::EventData::TabCreated { tab: tab.clone() },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::PaneCreated,
                            data: crate::api::schema::EventData::PaneCreated {
                                pane: root_pane.clone(),
                            },
                        });
                        SuccessResponse {
                            id: request.id,
                            result: self
                                .workspace_created_result(index)
                                .expect("new workspace should produce a complete create response"),
                        }
                    }
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_create_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
            }
            Method::WorkspaceFocus(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                if self.state.workspaces.get(index).is_none() {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                }
                self.state.switch_workspace(index);
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceRename(params) => {
                let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", params.workspace_id),
                        },
                    })
                    .unwrap();
                };
                let Some(ws) = self.state.workspaces.get_mut(index) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", params.workspace_id),
                        },
                    })
                    .unwrap();
                };
                ws.set_custom_name(params.label.clone());
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::WorkspaceRenamed,
                    data: crate::api::schema::EventData::WorkspaceRenamed {
                        workspace_id: self.public_workspace_id(index),
                        label: params.label,
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceClose(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                if self.state.workspaces.get(index).is_none() {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                }
                self.state.selected = index;
                self.state.close_selected_workspace();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::WorkspaceClosed,
                    data: crate::api::schema::EventData::WorkspaceClosed {
                        workspace_id: target.workspace_id,
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::TabList(TabListParams { workspace_id }) => {
                let tabs = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
                    };
                    let Some(ws) = self.state.workspaces.get(ws_idx) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
                    };
                    (0..ws.tabs.len())
                        .filter_map(|tab_idx| self.tab_info(ws_idx, tab_idx))
                        .collect()
                } else {
                    let mut tabs = Vec::new();
                    for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                        for tab_idx in 0..ws.tabs.len() {
                            if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                                tabs.push(tab);
                            }
                        }
                    }
                    tabs
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabList { tabs },
                }
            }
            Method::TabGet(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabCreate(params) => {
                let crate::api::schema::TabCreateParams {
                    workspace_id,
                    cwd,
                    focus,
                    label,
                } = params;
                let ws_idx = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
                    };
                    ws_idx
                } else if let Some(active) = self.state.active {
                    active
                } else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: "no active workspace".into(),
                        },
                    })
                    .unwrap();
                };
                let cwd = cwd
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        self.state.workspaces.get(ws_idx).and_then(|ws| {
                            ws.active_tab()
                                .and_then(|tab| tab.focused_runtime())
                                .and_then(|rt| rt.cwd())
                        })
                    })
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("/"));
                let (rows, cols) = self.state.estimate_pane_size();
                let result = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .ok_or_else(|| std::io::Error::other("workspace disappeared"))
                    .and_then(|ws| {
                        ws.create_tab(
                            rows,
                            cols,
                            cwd,
                            self.state.pane_scrollback_limit_bytes,
                            self.state.host_terminal_theme,
                        )
                    });
                match result {
                    Ok(tab_idx) => {
                        if let Some(label) = label {
                            if let Some(tab) = self
                                .state
                                .workspaces
                                .get_mut(ws_idx)
                                .and_then(|ws| ws.tabs.get_mut(tab_idx))
                            {
                                tab.set_custom_name(label);
                            }
                        }
                        if focus {
                            self.state.switch_workspace(ws_idx);
                            self.state.switch_tab(tab_idx);
                            self.state.mode = Mode::Terminal;
                        }
                        self.schedule_session_save();
                        let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                        let root_pane = self
                            .root_pane_info(ws_idx, tab_idx)
                            .expect("new tab should have a root pane");
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::TabCreated,
                            data: crate::api::schema::EventData::TabCreated { tab: tab.clone() },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::PaneCreated,
                            data: crate::api::schema::EventData::PaneCreated {
                                pane: root_pane.clone(),
                            },
                        });
                        SuccessResponse {
                            id: request.id,
                            result: self
                                .tab_created_result(ws_idx, tab_idx)
                                .expect("new tab should produce a complete create response"),
                        }
                    }
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "tab_create_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
            }
            Method::TabFocus(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                self.state.switch_workspace(ws_idx);
                self.state.switch_tab(tab_idx);
                let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabRename(params) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", params.tab_id),
                        },
                    })
                    .unwrap();
                };
                let Some(tab) = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", params.tab_id),
                        },
                    })
                    .unwrap();
                };
                tab.set_custom_name(params.label.clone());
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabRenamed,
                    data: crate::api::schema::EventData::TabRenamed {
                        tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                        workspace_id: self.public_workspace_id(ws_idx),
                        label: params.label,
                    },
                });
                let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabClose(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                if ws.tabs.len() <= 1 {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_close_failed".into(),
                            message: "cannot close the last tab in a workspace".into(),
                        },
                    })
                    .unwrap();
                }
                if !ws.close_tab(tab_idx) {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_close_failed".into(),
                            message: format!("tab {} could not be closed", target.tab_id),
                        },
                    })
                    .unwrap();
                }
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabClosed,
                    data: crate::api::schema::EventData::TabClosed {
                        tab_id: target.tab_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSplit(params) => {
                let Some((ws_idx, target_pane_id)) = self.parse_pane_id(&params.target_pane_id)
                else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.target_pane_id),
                        },
                    })
                    .unwrap();
                };
                let (rows, cols) = self.state.estimate_pane_size();
                let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.target_pane_id),
                        },
                    })
                    .unwrap();
                };
                ws.layout.focus_pane(target_pane_id);
                let direction = match params.direction {
                    crate::api::schema::SplitDirection::Right => {
                        ratatui::layout::Direction::Horizontal
                    }
                    crate::api::schema::SplitDirection::Down => {
                        ratatui::layout::Direction::Vertical
                    }
                };
                let new_pane_id = match ws.split_focused(
                    direction,
                    rows,
                    cols,
                    params.cwd.map(std::path::PathBuf::from),
                    self.state.pane_scrollback_limit_bytes,
                    self.state.host_terminal_theme,
                ) {
                    Ok(new_pane_id) => new_pane_id,
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_split_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                };
                if !params.focus {
                    ws.layout.focus_pane(target_pane_id);
                }
                self.schedule_session_save();
                let pane = self.pane_info(ws_idx, new_pane_id).unwrap();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneCreated,
                    data: crate::api::schema::EventData::PaneCreated { pane: pane.clone() },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneInfo { pane },
                }
            }
            Method::PaneList(PaneListParams { workspace_id }) => {
                match self.collect_panes_for_workspace(workspace_id.as_deref()) {
                    Ok(panes) => SuccessResponse {
                        id: request.id,
                        result: ResponseResult::PaneList { panes },
                    },
                    Err((code, message)) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody { code, message },
                        })
                        .unwrap();
                    }
                }
            }
            Method::PaneGet(target) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(pane) = self.pane_info(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneInfo { pane },
                }
            }
            Method::PaneRead(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(tab_idx) = self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
                else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
                let text = match params.source {
                    ReadSource::Visible => pane.visible_text(),
                    ReadSource::Recent => pane.recent_text(requested_lines),
                    ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneRead {
                        read: PaneReadResult {
                            pane_id: params.pane_id,
                            workspace_id,
                            tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                            source: params.source,
                            text,
                            revision: 0,
                            truncated: false,
                        },
                    },
                }
            }
            Method::PaneReportAgent(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(agent) = parse_agent_name(&params.agent) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "invalid_agent".into(),
                            message: format!("unsupported agent {}", params.agent),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: params.source,
                    agent,
                    state: detect_state_from_api(params.state),
                    message: params.message,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneClearAgentAuthority(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
                    pane_id,
                    source: params.source,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneReleaseAgent(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(agent) = parse_agent_name(&params.agent) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "invalid_agent".into(),
                            message: format!("unsupported agent {}", params.agent),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
                    pane_id,
                    source: params.source,
                    agent,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendText(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_send_failed".into(),
                            message: err.to_string(),
                        },
                    })
                    .unwrap();
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendInput(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let encoded_keys = match encode_api_keys(runtime, &params.keys) {
                    Ok(encoded_keys) => encoded_keys,
                    Err(key) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "invalid_key".into(),
                                message: format!("unsupported key {key}"),
                            },
                        })
                        .unwrap();
                    }
                };
                if !params.text.is_empty() {
                    let text_bytes = encode_api_text(runtime, &params.text);
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(text_bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                for bytes in encoded_keys {
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneClose(target) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                let workspace_id = self.state.workspaces[ws_idx].id.clone();
                let should_close_workspace = {
                    let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_not_found".into(),
                                message: format!("pane {} not found", target.pane_id),
                            },
                        })
                        .unwrap();
                    };
                    ws.close_pane(pane_id)
                };
                if should_close_workspace {
                    self.state.selected = ws_idx;
                    self.state.close_selected_workspace();
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneClosed,
                        data: crate::api::schema::EventData::PaneClosed {
                            pane_id: target.pane_id.clone(),
                            workspace_id: workspace_id.clone(),
                        },
                    });
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::WorkspaceClosed,
                        data: crate::api::schema::EventData::WorkspaceClosed { workspace_id },
                    });
                } else {
                    self.schedule_session_save();
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneClosed,
                        data: crate::api::schema::EventData::PaneClosed {
                            pane_id: target.pane_id,
                            workspace_id,
                        },
                    });
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendKeys(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let encoded_keys = match encode_api_keys(runtime, &params.keys) {
                    Ok(encoded_keys) => encoded_keys,
                    Err(key) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "invalid_key".into(),
                                message: format!("unsupported key {key}"),
                            },
                        })
                        .unwrap();
                    }
                };
                for bytes in encoded_keys {
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::IntegrationInstall(params) => {
                let target = params.target;
                let messages = match target {
                    IntegrationTarget::Pi => {
                        let path = crate::integration::install_pi().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_install_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match path {
                            Ok(path) => {
                                vec![format!("installed pi integration to {}", path.display())]
                            }
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Claude => {
                        let installed = crate::integration::install_claude().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_install_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match installed {
                            Ok(installed) => vec![
                                format!(
                                    "installed claude integration hook to {}",
                                    installed.hook_path.display()
                                ),
                                format!(
                                    "ensured claude settings at {}",
                                    installed.settings_path.display()
                                ),
                            ],
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Codex => {
                        let installed = crate::integration::install_codex().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_install_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match installed {
                            Ok(installed) => vec![
                                format!(
                                    "installed codex integration hook to {}",
                                    installed.hook_path.display()
                                ),
                                format!(
                                    "ensured codex hooks at {}",
                                    installed.hooks_path.display()
                                ),
                                format!(
                                    "ensured codex config at {}",
                                    installed.config_path.display()
                                ),
                            ],
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Opencode => {
                        let installed = crate::integration::install_opencode().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_install_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match installed {
                            Ok(installed) => vec![format!(
                                "installed opencode integration plugin to {}",
                                installed.plugin_path.display()
                            )],
                            Err(response) => return response,
                        }
                    }
                };

                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::IntegrationInstall {
                        target,
                        details: IntegrationInstallResult { messages },
                    },
                }
            }
            Method::IntegrationUninstall(params) => {
                let target = params.target;
                let messages = match target {
                    IntegrationTarget::Pi => {
                        let result = crate::integration::uninstall_pi().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_uninstall_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match result {
                            Ok(result) => {
                                if result.removed_extension {
                                    vec![format!(
                                        "removed pi integration extension at {}",
                                        result.extension_path.display()
                                    )]
                                } else {
                                    vec![format!(
                                        "no pi integration extension found at {}",
                                        result.extension_path.display()
                                    )]
                                }
                            }
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Claude => {
                        let result = crate::integration::uninstall_claude().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_uninstall_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match result {
                            Ok(result) => {
                                let mut messages = Vec::new();
                                if result.removed_hook_file {
                                    messages.push(format!(
                                        "removed claude hook at {}",
                                        result.hook_path.display()
                                    ));
                                } else {
                                    messages.push(format!(
                                        "no claude hook found at {}",
                                        result.hook_path.display()
                                    ));
                                }
                                if result.updated_settings {
                                    messages.push(format!(
                                        "removed herdr claude hook entries from {}",
                                        result.settings_path.display()
                                    ));
                                } else {
                                    messages.push(format!(
                                        "no herdr claude hook entries found in {}",
                                        result.settings_path.display()
                                    ));
                                }
                                messages
                            }
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Codex => {
                        let result = crate::integration::uninstall_codex().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_uninstall_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match result {
                            Ok(result) => {
                                let mut messages = Vec::new();
                                if result.removed_hook_file {
                                    messages.push(format!(
                                        "removed codex hook at {}",
                                        result.hook_path.display()
                                    ));
                                } else {
                                    messages.push(format!(
                                        "no codex hook found at {}",
                                        result.hook_path.display()
                                    ));
                                }
                                if result.updated_hooks {
                                    messages.push(format!(
                                        "removed herdr codex hook entries from {}",
                                        result.hooks_path.display()
                                    ));
                                } else {
                                    messages.push(format!(
                                        "no herdr codex hook entries found in {}",
                                        result.hooks_path.display()
                                    ));
                                }
                                messages.push(format!(
                                    "left codex config unchanged at {}",
                                    result.config_path.display()
                                ));
                                messages
                            }
                            Err(response) => return response,
                        }
                    }
                    IntegrationTarget::Opencode => {
                        let result = crate::integration::uninstall_opencode().map_err(|err| {
                            serde_json::to_string(&ErrorResponse {
                                id: request.id.clone(),
                                error: ErrorBody {
                                    code: "integration_uninstall_failed".into(),
                                    message: err.to_string(),
                                },
                            })
                            .unwrap()
                        });
                        match result {
                            Ok(result) => {
                                if result.removed_plugin {
                                    vec![format!(
                                        "removed opencode integration plugin at {}",
                                        result.plugin_path.display()
                                    )]
                                } else {
                                    vec![format!(
                                        "no opencode integration plugin found at {}",
                                        result.plugin_path.display()
                                    )]
                                }
                            }
                            Err(response) => return response,
                        }
                    }
                };

                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::IntegrationUninstall {
                        target,
                        details: IntegrationUninstallResult { messages },
                    },
                }
            }
            _ => {
                return serde_json::to_string(&ErrorResponse {
                    id: request.id,
                    error: ErrorBody {
                        code: "not_implemented".into(),
                        message: "method not implemented yet".into(),
                    },
                })
                .unwrap();
            }
        };

        serde_json::to_string(&response).unwrap()
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

    pub(crate) fn complete_onboarding(&mut self) {
        let (sound_enabled, toast_enabled) = match self.state.onboarding_list.selected {
            0 => (false, false),
            1 => (false, true),
            2 => (true, false),
            _ => (true, true),
        };

        match crate::config::save_onboarding_choices(sound_enabled, toast_enabled) {
            Ok(()) => {
                self.state.sound.enabled = sound_enabled;
                self.state.toast_config.enabled = toast_enabled;
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            Err(err) => {
                self.state.config_diagnostic =
                    Some(format!("failed to save onboarding config: {err}"));
                self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(8));
            }
        }
    }

    fn update_config_file<F>(&mut self, error_context: &str, update: F)
    where
        F: FnOnce(&str) -> String,
    {
        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
                return;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
        }
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

    fn save_theme(&mut self, name: &str) {
        self.update_config_file("theme", |content| {
            crate::config::upsert_section_value(content, "theme", "name", &format!("\"{name}\""))
        });
    }

    fn save_sound(&mut self, enabled: bool) {
        self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        });
    }

    fn save_toast(&mut self, enabled: bool) {
        self.update_config_file("toast setting", |content| {
            crate::config::upsert_section_bool(content, "ui.toast", "enabled", enabled)
        });
    }

    fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<std::path::PathBuf> {
        self.state.workspaces.get(ws_idx)?.resolved_identity_cwd()
    }

    fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    /// Create a workspace with a real PTY (needs event_tx).
    pub(crate) fn create_workspace(&mut self) {
        let initial_cwd = self
            .workspace_creation_source()
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        if let Err(e) = self.create_workspace_with_options(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.mode = Mode::Navigate;
        }
    }

    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let initial_cwd = self
            .state
            .active
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(tab_idx) => {
                if let Some(name) = custom_name {
                    if let Some(ws) = self
                        .state
                        .active
                        .and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    fn create_tab_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let ws = &mut self.state.workspaces[ws_idx];
        let idx = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        )?;
        if focus {
            ws.switch_tab(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    fn create_workspace_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let mut ws = Workspace::new(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        ws.refresh_git_ahead_behind();
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    fn tab_info(&self, ws_idx: usize, tab_idx: usize) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .map(|pane| (pane.state, pane.seen))
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab_idx + 1,
            label: tab.display_name(),
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: pane_agent_status(agg_state, seen),
        })
    }

    fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let runtime = ws.runtime(pane_id);
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: runtime
                .and_then(|rt| rt.cwd())
                .map(|cwd| cwd.display().to_string()),
            agent: pane.detected_agent.map(agent_name),
            agent_status: pane_agent_status(pane.state, pane.seen),
            revision: 0,
        })
    }

    fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::pane::PaneRuntime, String)> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let runtime = ws.runtime(pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::pane::PaneRuntime> {
        let ws = self.state.workspaces.get(ws_idx)?;
        ws.runtime(pane_id)
    }

    fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state();
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name(),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self
                .public_tab_id(index, ws.active_tab)
                .unwrap_or_else(|| format!("{}:{}", ws.id, ws.active_tab + 1)),
            agent_status: pane_agent_status(agg_state, seen),
        }
    }
}

fn tab_attention_priority(state: crate::detect::AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (crate::detect::AgentState::Blocked, _) => 4,
        (crate::detect::AgentState::Idle, false) => 3,
        (crate::detect::AgentState::Working, _) => 2,
        (crate::detect::AgentState::Idle, true) => 1,
        (crate::detect::AgentState::Unknown, _) => 0,
    }
}

fn parse_api_key(key: &str) -> Option<crossterm::event::KeyEvent> {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let normalized = key.trim();
    match normalized {
        "Enter" | "enter" => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
        "Tab" | "tab" => Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty())),
        "Esc" | "esc" => Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())),
        "Backspace" | "backspace" => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty())),
        "Up" | "up" => Some(KeyEvent::new(KeyCode::Up, KeyModifiers::empty())),
        "Down" | "down" => Some(KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
        "Left" | "left" => Some(KeyEvent::new(KeyCode::Left, KeyModifiers::empty())),
        "Right" | "right" => Some(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())),
        "C-c" | "c-c" | "ctrl+c" => Some(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        _ if normalized.len() == 1 => normalized
            .chars()
            .next()
            .map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())),
        _ => None,
    }
}

fn encode_api_text(runtime: &crate::pane::PaneRuntime, text: &str) -> Vec<u8> {
    let bracketed = runtime
        .input_state()
        .map(|state| state.bracketed_paste)
        .unwrap_or(false);
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

fn encode_api_keys(
    runtime: &crate::pane::PaneRuntime,
    keys: &[String],
) -> Result<Vec<Vec<u8>>, String> {
    let mut encoded_keys = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(key_event) = parse_api_key(key) else {
            return Err(key.clone());
        };
        encoded_keys.push(runtime.encode_terminal_key(key_event.into()));
    }
    Ok(encoded_keys)
}

fn detect_state_from_api(state: crate::api::schema::PaneAgentState) -> crate::detect::AgentState {
    match state {
        crate::api::schema::PaneAgentState::Idle => crate::detect::AgentState::Idle,
        crate::api::schema::PaneAgentState::Working => crate::detect::AgentState::Working,
        crate::api::schema::PaneAgentState::Blocked => crate::detect::AgentState::Blocked,
        crate::api::schema::PaneAgentState::Unknown => crate::detect::AgentState::Unknown,
    }
}

fn pane_agent_status(
    state: crate::detect::AgentState,
    seen: bool,
) -> crate::api::schema::AgentStatus {
    match (state, seen) {
        (crate::detect::AgentState::Idle, false) => crate::api::schema::AgentStatus::Done,
        (crate::detect::AgentState::Idle, true) => crate::api::schema::AgentStatus::Idle,
        (crate::detect::AgentState::Working, _) => crate::api::schema::AgentStatus::Working,
        (crate::detect::AgentState::Blocked, _) => crate::api::schema::AgentStatus::Blocked,
        (crate::detect::AgentState::Unknown, _) => crate::api::schema::AgentStatus::Unknown,
    }
}

fn parse_agent_name(agent: &str) -> Option<crate::detect::Agent> {
    match agent {
        "pi" => Some(crate::detect::Agent::Pi),
        "claude" => Some(crate::detect::Agent::Claude),
        "codex" => Some(crate::detect::Agent::Codex),
        "gemini" => Some(crate::detect::Agent::Gemini),
        "cursor" => Some(crate::detect::Agent::Cursor),
        "cline" => Some(crate::detect::Agent::Cline),
        "opencode" => Some(crate::detect::Agent::OpenCode),
        "copilot" => Some(crate::detect::Agent::GithubCopilot),
        "kimi" => Some(crate::detect::Agent::Kimi),
        "droid" => Some(crate::detect::Agent::Droid),
        "amp" => Some(crate::detect::Agent::Amp),
        _ => None,
    }
}

fn agent_name(agent: crate::detect::Agent) -> String {
    match agent {
        crate::detect::Agent::Pi => "pi",
        crate::detect::Agent::Claude => "claude",
        crate::detect::Agent::Codex => "codex",
        crate::detect::Agent::Gemini => "gemini",
        crate::detect::Agent::Cursor => "cursor",
        crate::detect::Agent::Cline => "cline",
        crate::detect::Agent::OpenCode => "opencode",
        crate::detect::Agent::GithubCopilot => "copilot",
        crate::detect::Agent::Kimi => "kimi",
        crate::detect::Agent::Droid => "droid",
        crate::detect::Agent::Amp => "amp",
    }
    .to_string()
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
    pub(crate) fn route_client_input(&mut self, data: Vec<u8>) {
        let events = crate::raw_input::parse_raw_input_bytes_sync(&data);

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
                    self.update_host_terminal_theme(kind, color);
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
                // Onboarding is handled by the App method since it may
                // need runtime access. For the server, skip onboarding.
            }
            Mode::ReleaseNotes => {
                // Release notes handling — simplified for server.
            }
            Mode::Settings => {
                // Settings handling — may need runtime access for saving.
                // For the server, we use a simplified approach.
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

        crate::release_notes::save_pending("0.5.0", "### Changed\n- One").unwrap();

        let app = test_app();

        assert_eq!(app.state.update_available.as_deref(), Some("0.5.0"));
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

        assert!(!api_request_changes_ui(&read_only));
        assert!(api_request_changes_ui(&mutating));
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
