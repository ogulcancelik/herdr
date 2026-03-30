//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

mod actions;
mod input;
pub mod state;

use std::future::pending;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
const ANIMATION_INTERVAL: Duration = Duration::from_millis(100);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GIT_REMOTE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
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
pub struct App {
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    event_rx: mpsc::Receiver<AppEvent>,
    api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
    event_hub: crate::api::EventHub,
    last_focus: Option<(usize, crate::layout::PaneId)>,
    no_session: bool,
    input_rx: Option<mpsc::Receiver<crate::raw_input::RawInputEvent>>,
    last_terminal_size: Option<(u16, u16)>,
    config_diagnostic_deadline: Option<Instant>,
    toast_deadline: Option<Instant>,
    last_git_remote_status_refresh: Instant,
    last_sidebar_divider_click: Option<Instant>,
    next_resize_poll: Instant,
    next_animation_tick: Option<Instant>,
    last_render_at: Option<Instant>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
}

enum LoopEvent {
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

fn api_request_changes_ui(request: &crate::api::schema::Request) -> bool {
    use crate::api::schema::Method;

    matches!(
        &request.method,
        Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceClose(_)
            | Method::PaneSplit(_)
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
        api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
        event_hub: crate::api::EventHub,
    ) -> Self {
        let (prefix_code, prefix_mods) = config.prefix_key();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));

        // Try to restore previous session
        let (workspaces, active, selected) = if no_session {
            (Vec::new(), None, 0)
        } else if let Some(snap) = crate::persist::load() {
            let ws = crate::persist::restore(
                &snap,
                24,
                80,
                event_tx.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            );
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

        let mode = if config.should_show_onboarding() {
            state::Mode::Onboarding
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
            request_new_workspace: false,
            request_complete_onboarding: false,
            name_input: String::new(),
            onboarding_step: 0,
            onboarding_selected: 1,
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
            config_diagnostic,
            toast: None,
            prefix_code,
            prefix_mods,
            sidebar_width: config.ui.sidebar_width,
            sidebar_width_auto: true,
            sidebar_collapsed: false,
            confirm_close: config.ui.confirm_close,
            confirm_close_selected_confirm: true,
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
                selected: 0,
                original_palette: None,
                original_theme: None,
            },
        };

        for ws in &mut state.workspaces {
            ws.refresh_git_ahead_behind();
        }

        // Background auto-update (skipped in --no-session / test mode)
        if !no_session {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }

        let last_focus = state.active.and_then(|idx| {
            state
                .workspaces
                .get(idx)
                .map(|ws| (idx, ws.layout.focused()))
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
            last_render_at: None,
            api_rx,
            event_hub,
            last_focus,
            no_session,
            input_rx: None,
            last_terminal_size: terminal::size().ok(),
            render_notify,
            render_dirty,
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        if self.input_rx.is_none() {
            self.input_rx = Some(crate::raw_input::spawn_input_reader());
        }

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

    fn drain_api_requests(&mut self) -> bool {
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
                if key.kind == crossterm::event::KeyEventKind::Press {
                    self.handle_key(key).await;
                    true
                } else {
                    false
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

    fn handle_scheduled_tasks(&mut self, now: Instant) -> bool {
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

        self.sync_animation_timer(now);
        changed
    }

    fn sync_animation_timer(&mut self, now: Instant) {
        if self.active_workspace_has_animation() {
            self.next_animation_tick
                .get_or_insert(now + ANIMATION_INTERVAL);
        } else {
            self.next_animation_tick = None;
        }
    }

    fn active_workspace_has_animation(&self) -> bool {
        self.state
            .active
            .and_then(|idx| self.state.workspaces.get(idx))
            .is_some_and(Workspace::has_working_pane)
    }

    fn can_render_now(&self, now: Instant) -> bool {
        match self.last_render_at {
            Some(last_render_at) => now.duration_since(last_render_at) >= MIN_RENDER_INTERVAL,
            None => true,
        }
    }

    fn git_refresh_deadline(&self) -> Option<Instant> {
        (!self.state.workspaces.is_empty())
            .then_some(self.last_git_remote_status_refresh + GIT_REMOTE_STATUS_REFRESH_INTERVAL)
    }

    fn next_loop_deadline(&self, now: Instant, needs_render: bool) -> Option<Instant> {
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
            render_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn drain_internal_events(&mut self) -> bool {
        let mut had_event = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            had_event = true;
            self.handle_internal_event(ev);
        }
        had_event
    }

    fn handle_internal_event(&mut self, ev: AppEvent) {
        match &ev {
            AppEvent::PaneDied { pane_id } => {
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
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
            } => {
                if let Some((ws_idx, pane)) = self.find_pane(*pane_id) {
                    if let Some(pane_id) = self.public_pane_id(ws_idx, *pane_id) {
                        let workspace_id = self.public_workspace_id(ws_idx);
                        if pane.detected_agent != *agent {
                            self.emit_event(crate::api::schema::EventEnvelope {
                                event: crate::api::schema::EventKind::PaneAgentDetected,
                                data: crate::api::schema::EventData::PaneAgentDetected {
                                    pane_id: pane_id.clone(),
                                    workspace_id: workspace_id.clone(),
                                    agent: agent.map(agent_name),
                                },
                            });
                        }
                        if pane.state != *state {
                            self.emit_event(crate::api::schema::EventEnvelope {
                                event: crate::api::schema::EventKind::PaneAgentStateChanged,
                                data: crate::api::schema::EventData::PaneAgentStateChanged {
                                    pane_id,
                                    workspace_id,
                                    state: pane_agent_state(*state),
                                },
                            });
                        }
                    }
                }
            }
            AppEvent::UpdateReady { .. } => {}
        }

        let previous_toast = self.state.toast.clone();
        self.state.handle_app_event(ev);
        if self.state.toast != previous_toast {
            self.toast_deadline = self.state.toast.as_ref().map(|toast| {
                let duration = match toast.kind {
                    ToastKind::NeedsAttention => Duration::from_secs(8),
                    ToastKind::Finished => Duration::from_secs(5),
                };
                Instant::now() + duration
            });
        }
    }

    fn emit_event(&self, event: crate::api::schema::EventEnvelope) {
        self.event_hub.push(event);
    }

    fn sync_focus_events(&mut self) {
        let current_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .map(|ws| (idx, ws.layout.focused()))
        });
        if current_focus == self.last_focus {
            return;
        }

        if let Some((ws_idx, pane_id)) = current_focus {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorkspaceFocused,
                data: crate::api::schema::EventData::WorkspaceFocused {
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
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

    fn find_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<(usize, &crate::pane::PaneState)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| ws.panes.get(&pane_id).map(|pane| (ws_idx, pane)))
    }

    fn public_workspace_id(&self, ws_idx: usize) -> String {
        (ws_idx + 1).to_string()
    }

    fn public_pane_id(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        Some(format!("{}-{pane_number}", ws_idx + 1))
    }

    fn parse_workspace_id(&self, id: &str) -> Option<usize> {
        if let Some(raw) = id.strip_prefix("w_") {
            return raw.parse::<usize>().ok()?.checked_sub(1);
        }
        id.parse::<usize>().ok()?.checked_sub(1)
    }

    fn parse_pane_id(&self, id: &str) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(rest) = id.strip_prefix("p_") {
            let (ws_raw, pane_raw) = rest.split_once('_')?;
            let ws_idx = ws_raw.parse::<usize>().ok()?.checked_sub(1)?;
            let pane_id = crate::layout::PaneId::from_raw(pane_raw.parse::<u32>().ok()?);
            return Some((ws_idx, pane_id));
        }

        let (ws_raw, pane_number_raw) = id.split_once('-')?;
        let ws_idx = ws_raw.parse::<usize>().ok()?.checked_sub(1)?;
        let pane_number = pane_number_raw.parse::<usize>().ok()?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws
            .public_pane_numbers
            .iter()
            .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
        Some((ws_idx, pane_id))
    }

    fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_internal_events();
        use bytes::Bytes;

        use crate::api::schema::{
            ErrorBody, ErrorResponse, Method, PaneListParams, PaneReadResult, ReadSource,
            ResponseResult, SuccessResponse,
        };

        let response = match request.method {
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
                        let workspace = self.workspace_info(index);
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::WorkspaceCreated,
                            data: crate::api::schema::EventData::WorkspaceCreated {
                                workspace: workspace.clone(),
                            },
                        });
                        if let Some(pane_id) = self.state.workspaces[index]
                            .layout
                            .pane_ids()
                            .first()
                            .copied()
                        {
                            if let Some(pane) = self.pane_info(index, pane_id) {
                                self.emit_event(crate::api::schema::EventEnvelope {
                                    event: crate::api::schema::EventKind::PaneCreated,
                                    data: crate::api::schema::EventData::PaneCreated { pane },
                                });
                            }
                        }
                        SuccessResponse {
                            id: request.id,
                            result: ResponseResult::WorkspaceInfo { workspace },
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
                let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
                let text = match params.source {
                    ReadSource::Visible => pane.visible_text(),
                    ReadSource::Recent => pane.recent_text(requested_lines),
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneRead {
                        read: PaneReadResult {
                            pane_id: params.pane_id,
                            workspace_id,
                            source: params.source,
                            text,
                            revision: 0,
                            truncated: false,
                        },
                    },
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
                if let Err(err) = runtime.0.try_send(Bytes::from(params.text)) {
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
                let workspace_id = format!("w_{}", ws_idx + 1);
                let pane_count = ws.layout.pane_count();
                if pane_count <= 1 {
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
                    ws.remove_pane(pane_id);
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
                for key in params.keys {
                    let Some(key_event) = parse_api_key(&key) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "invalid_key".into(),
                                message: format!("unsupported key {}", key),
                            },
                        })
                        .unwrap();
                    };
                    let flags = runtime
                        .1
                        .kitty_keyboard_flags
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let bytes = crate::input::encode_key(
                        key_event,
                        crate::input::KeyboardProtocol::from_kitty_flags(flags),
                    );
                    if let Err(err) = runtime.0.try_send(Bytes::from(bytes)) {
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

    pub(crate) fn complete_onboarding(&mut self) {
        let (sound_enabled, toast_enabled) = match self.state.onboarding_selected {
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

    /// Create a workspace with a real PTY (needs event_tx).
    fn create_workspace(&mut self) {
        let initial_cwd = self
            .state
            .active
            .and_then(|i| self.state.workspaces.get(i))
            .and_then(|ws| ws.focused_runtime())
            .and_then(|rt| rt.cwd())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        if let Err(e) = self.create_workspace_with_options(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.mode = Mode::Navigate;
        }
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
                .layout
                .pane_ids()
                .into_iter()
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.panes.get(&pane_id)?;
        let runtime = ws.runtimes.get(&pane_id);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            focused: self.state.active == Some(ws_idx) && ws.layout.focused() == pane_id,
            cwd: runtime
                .and_then(|rt| rt.cwd())
                .map(|cwd| cwd.display().to_string()),
            agent: pane.detected_agent.map(agent_name),
            agent_state: pane_agent_state(pane.state),
            revision: 0,
        })
    }

    fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::pane::PaneRuntime, String)> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let runtime = ws.runtimes.get(&pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(
        &tokio::sync::mpsc::Sender<bytes::Bytes>,
        &crate::pane::PaneRuntime,
    )> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let runtime = ws.runtimes.get(&pane_id)?;
        Some((&runtime.sender, runtime))
    }

    fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, _) = ws.aggregate_state();
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name(),
            focused: self.state.active == Some(index),
            pane_count: ws.panes.len(),
            agent_state: pane_agent_state(agg_state),
        }
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

fn pane_agent_state(state: crate::detect::AgentState) -> crate::api::schema::PaneAgentState {
    match state {
        crate::detect::AgentState::Idle => crate::api::schema::PaneAgentState::Idle,
        crate::detect::AgentState::Working => crate::api::schema::PaneAgentState::Working,
        crate::detect::AgentState::Blocked => crate::api::schema::PaneAgentState::Blocked,
        crate::detect::AgentState::Unknown => crate::api::schema::PaneAgentState::Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_input_waits_when_reader_is_gone() {
        let result =
            tokio::time::timeout(Duration::from_millis(20), recv_raw_input_or_pending(None)).await;
        assert!(result.is_err());
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
}
