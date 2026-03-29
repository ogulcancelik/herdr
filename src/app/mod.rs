//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

mod actions;
mod input;
pub mod state;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::config::Config;
use crate::events::AppEvent;
use crate::workspace::Workspace;

pub use state::{AppState, Mode, ToastKind, ViewState, CONTEXT_MENU_ITEMS};

/// Full application: AppState + runtime concerns (event channels, async I/O).
pub struct App {
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    event_rx: mpsc::Receiver<AppEvent>,
    api_rx: std::sync::mpsc::Receiver<crate::api::ApiRequestMessage>,
    no_session: bool,
    config_diagnostic_deadline: Option<Instant>,
    toast_deadline: Option<Instant>,
}

impl App {
    pub fn new(
        config: &Config,
        no_session: bool,
        config_diagnostic: Option<String>,
        api_rx: std::sync::mpsc::Receiver<crate::api::ApiRequestMessage>,
    ) -> Self {
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

        let mode = if config.should_show_onboarding() {
            state::Mode::Onboarding
        } else if active.is_some() {
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
            sidebar_collapsed: false,
            confirm_close: config.ui.confirm_close,
            accent: crate::config::parse_color(&config.ui.accent),
            sound: config.ui.sound.clone(),
            toast_config: config.ui.toast.clone(),
            keybinds: config.keybinds(),
        };

        // Background auto-update (skipped in --no-session / test mode)
        if !no_session {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }

        Self {
            config_diagnostic_deadline: state
                .config_diagnostic
                .as_ref()
                .map(|_| Instant::now() + Duration::from_secs(8)),
            toast_deadline: None,
            state,
            event_tx,
            event_rx,
            api_rx,
            no_session,
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.state.should_quit {
            if self
                .config_diagnostic_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.config_diagnostic_deadline = None;
                self.state.config_diagnostic = None;
            }

            if self
                .toast_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.toast_deadline = None;
                self.state.toast = None;
            }

            terminal.draw(|frame| {
                crate::ui::compute_view(&mut self.state, frame.area());
                crate::ui::render(&self.state, frame);
            })?;

            while let Ok(msg) = self.api_rx.try_recv() {
                let response = self.handle_api_request(msg.request);
                let _ = msg.respond_to.send(response);
            }

            // Drain internal events
            while let Ok(ev) = self.event_rx.try_recv() {
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

            if self.state.request_complete_onboarding {
                self.state.request_complete_onboarding = false;
                self.complete_onboarding();
            }

            if self.state.request_new_workspace {
                self.state.request_new_workspace = false;
                self.create_workspace();
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

    fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
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
                        .map(|(idx, _)| workspace_info(&self.state, idx))
                        .collect(),
                },
            },
            Method::WorkspaceGet(target) => {
                let Some(index) = parse_workspace_id(&target.workspace_id) else {
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
                        workspace: workspace_info(&self.state, index),
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
                    Ok(index) => SuccessResponse {
                        id: request.id,
                        result: ResponseResult::WorkspaceInfo {
                            workspace: workspace_info(&self.state, index),
                        },
                    },
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
                let Some((ws_idx, pane_id)) = parse_pane_id(&target.pane_id) else {
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
                let Some((ws_idx, pane_id)) = parse_pane_id(&params.pane_id) else {
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
                let Some((ws_idx, pane_id)) = parse_pane_id(&params.pane_id) else {
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
            Method::PaneSendKeys(params) => {
                let Some((ws_idx, pane_id)) = parse_pane_id(&params.pane_id) else {
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
                    let kitty = runtime
                        .1
                        .kitty_keyboard
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let bytes = crate::input::encode_key(key_event, kitty);
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
        let ws = Workspace::new(initial_cwd, rows, cols, self.event_tx.clone())?;
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
            let Some(ws_idx) = parse_workspace_id(workspace_id) else {
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
            pane_id: format!("p_{}_{}", ws_idx + 1, pane_id.raw()),
            workspace_id: format!("w_{}", ws_idx + 1),
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
        Some((runtime, format!("w_{}", ws_idx + 1)))
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

fn parse_workspace_id(id: &str) -> Option<usize> {
    id.strip_prefix("w_")?.parse::<usize>().ok()?.checked_sub(1)
}

fn parse_pane_id(id: &str) -> Option<(usize, crate::layout::PaneId)> {
    let rest = id.strip_prefix("p_")?;
    let (ws_raw, pane_raw) = rest.split_once('_')?;
    let ws_idx = ws_raw.parse::<usize>().ok()?.checked_sub(1)?;
    let pane_id = crate::layout::PaneId::from_raw(pane_raw.parse::<u32>().ok()?);
    Some((ws_idx, pane_id))
}

fn pane_agent_state(state: crate::detect::AgentState) -> crate::api::schema::PaneAgentState {
    match state {
        crate::detect::AgentState::Idle => crate::api::schema::PaneAgentState::Idle,
        crate::detect::AgentState::Busy => crate::api::schema::PaneAgentState::Busy,
        crate::detect::AgentState::Waiting => crate::api::schema::PaneAgentState::Waiting,
        crate::detect::AgentState::Unknown => crate::api::schema::PaneAgentState::Unknown,
    }
}

fn workspace_info(state: &AppState, index: usize) -> crate::api::schema::WorkspaceInfo {
    let ws = &state.workspaces[index];
    let (agg_state, _) = ws.aggregate_state();
    crate::api::schema::WorkspaceInfo {
        workspace_id: format!("w_{}", index + 1),
        number: index + 1,
        label: ws.display_name(),
        focused: state.active == Some(index),
        pane_count: ws.panes.len(),
        agent_state: pane_agent_state(agg_state),
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
