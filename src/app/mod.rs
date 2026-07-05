//! Application orchestration.
//!
//! - `state.rs` — AppState, Mode, and pure data structs
//! - `actions.rs` — state mutations (testable without PTYs/async)
//! - `input.rs` — key/mouse → action translation

pub(crate) mod actions;
mod agent_resume;
mod agents;
mod api;
mod api_helpers;
mod config_io;
mod creation;
mod ids;
mod input;
mod runtime;
mod runtime_mutations;
mod session;
pub mod state;
mod terminal_targets;
mod theme_sync;
mod worktrees;

use std::collections::{HashMap, HashSet};
use std::future::pending;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
pub(crate) const HEADLESS_ANIMATION_INTERVAL: Duration = Duration::from_millis(128);
pub(crate) const HEADLESS_ANIMATION_TICK_STEP: u32 = 8;
pub(crate) const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(30);
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GIT_REMOTE_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(1500);
const AUTO_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const PENDING_AGENT_RESUME_THEME_WAIT: Duration = Duration::from_millis(750);
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);
const SIDEBAR_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
const PANE_DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(350);
const PANE_COPY_HIGHLIGHT_DURATION: Duration = Duration::from_millis(500);
const COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(2);

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute, terminal,
};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use tokio::sync::{mpsc, Notify};
use tracing::info;

use crate::config::Config;
use crate::events::AppEvent;

pub use state::{AppState, Mode, ToastKind, ViewState};

pub(crate) fn load_plugin_manifest(
    path: &str,
    enabled: bool,
) -> Result<crate::api::schema::InstalledPluginInfo, (&'static str, String)> {
    api::plugins::load_plugin_manifest(path, enabled)
}

/// Full application: AppState + runtime concerns (event channels, async I/O).
#[derive(Debug, Clone)]
pub(crate) struct OverlayPaneState {
    ws_idx: usize,
    tab_idx: usize,
    previous_focus: crate::layout::PaneId,
    previous_zoomed: bool,
    temp_files: Vec<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PaneClickState {
    pane_id: crate::layout::PaneId,
    viewport_row: u16,
    col: u16,
    at: Instant,
}

impl PaneClickState {
    fn is_double_click_for(self, next: Self) -> bool {
        self.pane_id == next.pane_id
            && next.at.duration_since(self.at) <= PANE_DOUBLE_CLICK_WINDOW
            && self.viewport_row.abs_diff(next.viewport_row) <= 1
            && self.col.abs_diff(next.col) <= 1
    }
}

pub struct App {
    pub state: AppState,
    pub(crate) terminal_runtimes: crate::terminal::TerminalRuntimeRegistry,
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
    pub(crate) copy_feedback_deadline: Option<Instant>,
    pub(crate) last_api_notification_at: Option<Instant>,
    pub(crate) last_git_remote_status_refresh: Instant,
    pub(crate) git_refresh_in_flight: bool,
    pub(crate) git_refresh_due_after_in_flight: bool,
    pub(crate) git_status_cache: HashMap<std::path::PathBuf, crate::workspace::GitStatusCacheEntry>,
    pub(crate) pending_api_worktree_creates: HashMap<std::path::PathBuf, u64>,
    pub(crate) pending_api_worktree_removes: HashMap<String, u64>,
    pub(crate) pending_api_worktree_remove_paths: HashMap<std::path::PathBuf, u64>,
    pub(crate) next_api_worktree_operation_id: u64,
    pub(crate) last_sidebar_divider_click: Option<Instant>,
    pub(crate) last_pane_click: Option<PaneClickState>,
    pub(crate) next_resize_poll: Instant,
    pub(crate) next_animation_tick: Option<Instant>,
    pub(crate) next_auto_update_check: Option<Instant>,
    pub(crate) next_agent_manifest_update_check: Option<Instant>,
    pub(crate) update_version_check_enabled: bool,
    pub(crate) update_manifest_check_enabled: bool,
    pub(crate) loaded_host_cursor: crate::config::HostCursorModeConfig,
    pub(crate) agent_metadata_deadline: Option<Instant>,
    pub(crate) pending_agent_resume_deadline: Option<Instant>,
    pub(crate) selection_autoscroll_deadline: Option<Instant>,
    pub(crate) selection_highlight_clear_deadline: Option<Instant>,
    pub(crate) session_save_deadline: Option<Instant>,
    pub(crate) persist_pane_history: bool,
    pub(crate) last_render_at: Option<Instant>,
    pub(crate) suppressed_repeat_keys:
        HashSet<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    pub render_notify: Arc<Notify>,
    pub render_dirty: Arc<AtomicBool>,
    pub(crate) full_redraw_pending: bool,
    pub(crate) overlay_panes: HashMap<crate::layout::PaneId, OverlayPaneState>,
    pub(crate) local_terminal_notifications: bool,
    /// Whether this process applies `AppEvent::PrefixInputSource` to the host input source.
    /// The headless server sets this to false: the switch belongs to the foreground client,
    /// even when an App-internal drain consumes the event before the forwarding drain.
    pub(crate) local_input_source_switch: bool,
    pub(crate) config_reloaded_from_disk: bool,
    prefix_input_source: Box<dyn crate::platform::PrefixInputSource>,
}

pub(crate) const APP_EVENT_CHANNEL_CAPACITY: usize = 256;
pub(crate) const APP_EVENT_DRAIN_LIMIT: usize = 64;

pub(crate) enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(Box<crate::api::ApiRequestMessage>),
    RawInput(crate::raw_input::RawInputEvent),
    InputClosed,
    RenderRequested,
}

struct SyncOutputGuard;

impl SyncOutputGuard {
    fn begin() -> io::Result<Self> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(b"\x1b[?2026h")?;
        stdout.flush()?;
        Ok(Self)
    }
}

impl Drop for SyncOutputGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(b"\x1b[?2026l");
        let _ = stdout.flush();
    }
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

fn background_update_check_enabled(no_session: bool, check_enabled: bool) -> bool {
    auto_updates_enabled(no_session) && check_enabled
}

fn load_plugin_registry(no_session: bool) -> crate::app::state::InstalledPluginRegistry {
    if no_session {
        return std::collections::HashMap::new();
    }
    let entries = crate::persist::plugin_registry::load();
    let entries = crate::persist::plugin_registry::reload_manifests(entries, |path, enabled| {
        crate::app::api::plugins::load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
    });
    entries
        .into_iter()
        .map(|plugin| (plugin.plugin_id.clone(), plugin))
        .collect()
}

fn agent_panel_sort_from_config(
    sort: crate::config::AgentPanelSortConfig,
) -> state::AgentPanelSort {
    match sort {
        crate::config::AgentPanelSortConfig::Spaces => state::AgentPanelSort::Spaces,
        crate::config::AgentPanelSortConfig::Priority => state::AgentPanelSort::Priority,
    }
}

/// Parse the configured agent name list into a deduplicated set of `Agent`
/// values. Unknown agent names are silently dropped so a typo cannot disable
/// other valid entries.
fn parse_cjk_ime_agents(names: &[String]) -> Vec<crate::detect::Agent> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(agent) = crate::detect::parse_agent_label(name) {
            if !out.contains(&agent) {
                out.push(agent);
            }
        }
    }
    out
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn sibling_theme_names(name: &str) -> (String, String) {
    match normalize_theme_name(name).as_str() {
        "catppuccin" | "catppuccin-mocha" | "catppuccin-latte" | "latte" | "light" => {
            ("catppuccin".to_string(), "catppuccin-latte".to_string())
        }
        "tokyo-night" | "tokyonight" | "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => {
            ("tokyo-night".to_string(), "tokyo-night-day".to_string())
        }
        "gruvbox" | "gruvbox-dark" | "gruvbox-light" => {
            ("gruvbox".to_string(), "gruvbox-light".to_string())
        }
        "one-dark" | "onedark" | "one-light" | "onelight" => {
            ("one-dark".to_string(), "one-light".to_string())
        }
        "solarized" | "solarized-dark" | "solarized-light" => {
            ("solarized".to_string(), "solarized-light".to_string())
        }
        "kanagawa" | "kanagawa-lotus" | "lotus" => {
            ("kanagawa".to_string(), "kanagawa-lotus".to_string())
        }
        "rose-pine" | "rosepine" | "rose-pine-dawn" | "rosepine-dawn" | "dawn" => {
            ("rose-pine".to_string(), "rose-pine-dawn".to_string())
        }
        _ => (name.to_string(), name.to_string()),
    }
}

fn theme_runtime_config(
    config: &crate::config::Config,
    use_legacy_ui_accent: bool,
) -> state::ThemeRuntimeConfig {
    let manual_name = config
        .theme
        .name
        .clone()
        .unwrap_or_else(|| "catppuccin".to_string());
    let (default_dark, default_light) = sibling_theme_names(&manual_name);
    state::ThemeRuntimeConfig {
        manual_name,
        dark_name: config.theme.dark_name.clone().unwrap_or(default_dark),
        light_name: config.theme.light_name.clone().unwrap_or(default_light),
        auto_switch: config.theme.auto_switch,
        custom: config.theme.custom.clone(),
        legacy_accent: (use_legacy_ui_accent
            && config.ui.accent != "cyan"
            && config
                .theme
                .custom
                .as_ref()
                .and_then(|c| c.accent.as_ref())
                .is_none())
        .then(|| config.ui.accent.clone()),
    }
}

fn resolve_palette_for_theme_name(
    name: &str,
    fallback_name: &str,
    runtime: &state::ThemeRuntimeConfig,
) -> state::Palette {
    let mut palette = state::Palette::from_name(name).unwrap_or_else(|| {
        tracing::warn!(
            theme = name,
            fallback = fallback_name,
            "unknown theme, falling back"
        );
        state::Palette::from_name(fallback_name).unwrap_or_else(state::Palette::catppuccin)
    });

    if let Some(custom) = &runtime.custom {
        palette = palette.with_overrides(custom);
    }
    if let Some(accent) = &runtime.legacy_accent {
        palette.accent = crate::config::parse_color(accent);
    }

    palette
}

fn resolve_effective_theme(
    runtime: &state::ThemeRuntimeConfig,
    appearance: Option<crate::terminal_theme::HostAppearance>,
) -> (state::Palette, String) {
    let (name, fallback) = if runtime.auto_switch {
        match appearance.unwrap_or(crate::terminal_theme::HostAppearance::Dark) {
            crate::terminal_theme::HostAppearance::Dark => (&runtime.dark_name, "catppuccin"),
            crate::terminal_theme::HostAppearance::Light => {
                (&runtime.light_name, "catppuccin-latte")
            }
        }
    } else {
        (&runtime.manual_name, "catppuccin")
    };
    (
        resolve_palette_for_theme_name(name, fallback, runtime),
        name.clone(),
    )
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
        crate::kitty_graphics::set_enabled(config.experimental.kitty_graphics);
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>(APP_EVENT_CHANNEL_CAPACITY);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));

        // Try to restore previous session
        let mut restored_terminals = std::collections::HashMap::new();
        let mut restored_terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let (
            workspaces,
            active,
            selected,
            sidebar_width,
            sidebar_width_source,
            sidebar_section_split,
            collapsed_space_keys,
        ) = if no_session {
            (
                Vec::new(),
                None,
                0,
                config.ui.sidebar_width,
                state::SidebarWidthSource::ConfigDefault,
                0.5_f32,
                std::collections::HashSet::new(),
            )
        } else if let Some(snap) = crate::persist::load() {
            let history = config
                .experimental
                .pane_history
                .then(crate::persist::load_history)
                .flatten();
            let (ws, terminals, terminal_runtimes) = crate::persist::restore(
                &snap,
                history.as_ref(),
                24,
                80,
                config.advanced.scrollback_limit_bytes,
                &config.terminal.default_shell,
                config.terminal.shell_mode,
                config.session.resume_agents_on_restore,
                event_tx.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            );
            restored_terminals = terminals;
            restored_terminal_runtimes = terminal_runtimes.into();
            if ws.is_empty() {
                crate::logging::session_restored(0, "empty");
                (
                    Vec::new(),
                    None,
                    0,
                    snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                    if snap.sidebar_width.is_some() {
                        state::SidebarWidthSource::Persisted
                    } else {
                        state::SidebarWidthSource::ConfigDefault
                    },
                    snap.sidebar_section_split.unwrap_or(0.5),
                    snap.collapsed_space_keys,
                )
            } else {
                crate::logging::session_restored(ws.len(), "ok");
                let active = snap.active.filter(|&i| i < ws.len());
                let selected = snap.selected.min(ws.len().saturating_sub(1));
                (
                    ws,
                    active,
                    selected,
                    snap.sidebar_width.unwrap_or(config.ui.sidebar_width),
                    if snap.sidebar_width.is_some() {
                        state::SidebarWidthSource::Persisted
                    } else {
                        state::SidebarWidthSource::ConfigDefault
                    },
                    snap.sidebar_section_split.unwrap_or(0.5),
                    snap.collapsed_space_keys,
                )
            }
        } else {
            (
                Vec::new(),
                None,
                0,
                config.ui.sidebar_width,
                state::SidebarWidthSource::ConfigDefault,
                0.5_f32,
                std::collections::HashSet::new(),
            )
        };

        let agent_panel_sort = agent_panel_sort_from_config(config.ui.agent_panel_sort);

        // Validate sidebar bounds before they reach any `u16::clamp(min, max)`
        // call: `clamp` panics when `min > max`. On bad config, fall back to
        // the built-in defaults rather than crashing on the first render.
        let (sidebar_min_width, sidebar_max_width) = crate::config::validated_sidebar_bounds(
            config.ui.sidebar_min_width,
            config.ui.sidebar_max_width,
        )
        .unwrap_or_else(|| {
            tracing::warn!(
                min = config.ui.sidebar_min_width,
                max = config.ui.sidebar_max_width,
                "ui.sidebar_min_width is greater than sidebar_max_width; falling back to default bounds (18, 36)"
            );
            (18, 36)
        });

        let worktree_directory =
            crate::worktree::expand_tilde_absolute_path(&config.worktrees.directory);

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
        let update_install_command = crate::update::update_install_command().to_string();
        let startup_product_announcement =
            crate::product_announcements::load_unseen_for_current_version();

        let mode = if config.should_show_onboarding() {
            state::Mode::Onboarding
        } else if startup_product_announcement.is_some() {
            state::Mode::ProductAnnouncement
        } else if active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        };

        let agent_manifest_summaries = crate::detect::manifest::reload_manifests();
        let theme_runtime = theme_runtime_config(config, true);
        let (theme_palette, theme_name) = resolve_effective_theme(&theme_runtime, None);

        let mut state = AppState {
            terminals: std::collections::HashMap::new(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            pane_id_aliases: std::collections::HashMap::new(),
            public_pane_id_aliases: std::collections::HashMap::new(),
            workspaces,
            active,
            previous_pane_focus: None,
            selected,
            mode,
            should_quit: false,
            detach_exits: no_session,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_new_linked_worktree: None,
            request_open_existing_worktree: None,
            request_new_workspace_cwd: None,
            request_remove_linked_worktree: None,
            request_submit_worktree_create: false,
            request_submit_worktree_open: false,
            request_submit_worktree_remove: false,
            request_reload_config: false,
            request_client_config_reload: false,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            rename_pane_target: None,
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            worktree_directory,
            collapsed_space_keys,
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            release_notes: None,
            product_announcement: startup_product_announcement.map(|announcement| {
                state::ProductAnnouncementState {
                    version: announcement.version,
                    id: announcement.id,
                    title: announcement.title,
                    body: announcement.body,
                    scroll: 0,
                    preview: announcement.preview,
                }
            }),
            keybind_help: state::KeybindHelpState { scroll: 0 },
            navigator: state::NavigatorState::default(),
            copy_mode: None,
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: state::ViewState {
                layout: state::ViewLayout::Desktop,
                sidebar_rect: Rect::default(),
                workspace_card_areas: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            workspace_press: None,
            tab_press: None,
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            update_available,
            update_install_command,
            latest_release_notes_available,
            update_dismissed: false,
            config_diagnostic,
            toast: None,
            pending_agent_notifications: std::collections::HashMap::new(),
            copy_feedback: None,
            outer_terminal_focus: None,
            prefix_code,
            prefix_mods,
            default_sidebar_width: config.ui.sidebar_width,
            sidebar_width,
            sidebar_min_width,
            sidebar_max_width,
            mobile_width_threshold: config.ui.mobile_width_threshold,
            sidebar_width_source,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            sidebar_collapsed_mode: config.ui.sidebar_collapsed_mode,
            sidebar_section_split,
            agent_panel_sort,
            next_agent_state_change_seq: 0,
            mouse_capture: config.ui.mouse_capture,
            right_click_passthrough_modifiers: config.ui.right_click_passthrough_modifiers(),
            right_click_passthrough: None,
            redraw_on_focus_gained: config.ui.redraw_on_focus_gained,
            mouse_scroll_lines: config.ui.mouse_scroll_lines(),
            confirm_close: config.ui.confirm_close,
            prompt_new_tab_name: config.ui.prompt_new_tab_name,
            pane_borders: config.ui.pane_borders,
            pane_gaps: config.ui.pane_gaps,
            show_agent_labels_on_pane_borders: config.ui.show_agent_labels_on_pane_borders,
            hide_tab_bar_when_single_tab: config.ui.hide_tab_bar_when_single_tab,
            pane_history_persistence: config.experimental.pane_history,
            reveal_hidden_cursor_for_cjk_ime: config.experimental.reveal_hidden_cursor_for_cjk_ime,
            cjk_ime_agent_filter_configured: !config.experimental.cjk_ime_agents.is_empty(),
            cjk_ime_agents: parse_cjk_ime_agents(&config.experimental.cjk_ime_agents),
            cjk_ime_cursor_shape: config.experimental.cjk_ime_cursor_shape.to_decscusr(),
            switch_ascii_input_source_in_prefix: config
                .experimental
                .switch_ascii_input_source_in_prefix,
            kitty_graphics_enabled: config.experimental.kitty_graphics,
            default_shell: config.terminal.default_shell.clone(),
            shell_mode: config.terminal.shell_mode,
            new_terminal_cwd: config.terminal.new_cwd.clone(),
            pane_scrollback_limit_bytes: config.advanced.scrollback_limit_bytes,
            accent: crate::config::parse_color(&config.ui.accent),
            sound: config.ui.sound.clone(),
            local_sound_playback: true,
            toast_config: config.ui.toast.clone(),
            keybinds: config.keybinds(),
            spinner_tick: 0,
            palette: theme_palette,
            theme_name,
            theme_runtime,
            host_terminal_appearance: None,
            host_terminal_appearance_explicit: false,
            settings: state::SettingsState {
                section: state::SettingsSection::Theme,
                list: state::SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
            },
            integration_recommendations: crate::integration::integration_recommendations(),
            agent_manifest_summaries,
            agent_manifest_update_status: crate::detect::manifest_update::load_status(),
            integration_install_messages: Vec::new(),
            installed_plugins: load_plugin_registry(no_session),
            plugin_panes: std::collections::HashMap::new(),
            plugin_command_logs: Vec::new(),
            next_plugin_command_log_id: 1,
            plugin_commands_in_flight: 0,
            global_menu: state::MenuListState::new(0),
            host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
            session_dirty: false,
            terminal_runtime_shutdowns: Vec::new(),
        };

        state.terminals = restored_terminals;

        for ws_idx in 0..state.workspaces.len() {
            let cwd = state.workspaces[ws_idx]
                .resolved_identity_cwd_from(&state.terminals, &restored_terminal_runtimes);
            state.workspaces[ws_idx].cached_git_branch =
                cwd.as_deref().and_then(crate::workspace::git_branch);
        }

        // Background auto-update is disabled in monolithic no-session mode
        // and in debug/test builds so local development never mutates the
        // running binary out from under spawned test processes.
        let version_check_enabled =
            background_update_check_enabled(no_session, config.update.version_check);
        let manifest_check_enabled =
            background_update_check_enabled(no_session, config.update.manifest_check);
        if version_check_enabled {
            let update_tx = event_tx.clone();
            std::thread::spawn(move || crate::update::auto_update(update_tx));
        }
        if manifest_check_enabled {
            let manifest_update_tx = event_tx.clone();
            std::thread::spawn(move || {
                crate::detect::manifest_update::auto_update(manifest_update_tx)
            });
        }

        let last_focus = state.active.and_then(|idx| {
            state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });

        Self {
            config_diagnostic_deadline: None,
            toast_deadline: None,
            copy_feedback_deadline: None,
            last_api_notification_at: None,
            state,
            terminal_runtimes: restored_terminal_runtimes,
            event_tx,
            event_rx,
            last_git_remote_status_refresh: Instant::now() - GIT_REMOTE_STATUS_REFRESH_INTERVAL,
            git_refresh_in_flight: false,
            git_refresh_due_after_in_flight: false,
            git_status_cache: HashMap::new(),
            pending_api_worktree_creates: HashMap::new(),
            pending_api_worktree_removes: HashMap::new(),
            pending_api_worktree_remove_paths: HashMap::new(),
            next_api_worktree_operation_id: 1,
            last_sidebar_divider_click: None,
            last_pane_click: None,
            next_resize_poll: Instant::now() + RESIZE_POLL_INTERVAL,
            next_animation_tick: None,
            next_auto_update_check: version_check_enabled
                .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL),
            next_agent_manifest_update_check: manifest_check_enabled
                .then_some(Instant::now() + AUTO_UPDATE_CHECK_INTERVAL),
            update_version_check_enabled: config.update.version_check,
            update_manifest_check_enabled: config.update.manifest_check,
            loaded_host_cursor: config.ui.host_cursor,
            agent_metadata_deadline: None,
            pending_agent_resume_deadline: None,
            session_save_deadline: None,
            selection_autoscroll_deadline: None,
            selection_highlight_clear_deadline: None,
            persist_pane_history: config.experimental.pane_history,
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
            full_redraw_pending: false,
            overlay_panes: HashMap::new(),
            local_terminal_notifications: true,
            local_input_source_switch: true,
            config_reloaded_from_disk: false,
            prefix_input_source: Box::new(crate::platform::RealPrefixInputSource::default()),
        }
    }

    #[cfg(unix)]
    pub fn new_from_handoff(
        config: &Config,
        config_diagnostic: Option<String>,
        api_rx: tokio::sync::mpsc::UnboundedReceiver<crate::api::ApiRequestMessage>,
        event_hub: crate::api::EventHub,
        snapshot: &crate::persist::SessionSnapshot,
        imports: &mut std::collections::HashMap<
            u32,
            crate::handoff_runtime::ImportedHandoffRuntime,
        >,
    ) -> io::Result<Self> {
        let mut app = Self::new(config, true, config_diagnostic, api_rx, event_hub);
        let (workspaces, terminals, runtimes) = crate::persist::restore_handoff(
            snapshot,
            config.advanced.scrollback_limit_bytes,
            &config.terminal.default_shell,
            config.terminal.shell_mode,
            imports,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )?;
        let pane_id_aliases = crate::persist::handoff_pane_aliases(snapshot, &workspaces);

        app.no_session = false;
        let now = Instant::now();
        if background_update_check_enabled(app.no_session, app.update_version_check_enabled) {
            app.next_auto_update_check = app
                .state
                .update_available
                .is_none()
                .then_some(now + AUTO_UPDATE_CHECK_INTERVAL);
        }
        if background_update_check_enabled(app.no_session, app.update_manifest_check_enabled) {
            app.next_agent_manifest_update_check = Some(now + AUTO_UPDATE_CHECK_INTERVAL);
        }
        app.state.detach_exits = false;
        app.state.pane_id_aliases = pane_id_aliases;
        app.state.workspaces = workspaces;
        app.state.terminals = terminals;
        app.terminal_runtimes = runtimes.into();
        app.state.active = snapshot
            .active
            .filter(|&idx| idx < app.state.workspaces.len());
        app.state.selected = snapshot
            .selected
            .min(app.state.workspaces.len().saturating_sub(1));
        if let Some(width) = snapshot.sidebar_width {
            app.state.sidebar_width = width;
            app.state.sidebar_width_source = state::SidebarWidthSource::Persisted;
        }
        if let Some(split) = snapshot.sidebar_section_split {
            app.state.sidebar_section_split = split;
        }
        app.state.collapsed_space_keys = snapshot.collapsed_space_keys.clone();
        app.state.mode = if app.state.active.is_some() {
            state::Mode::Terminal
        } else {
            state::Mode::Navigate
        };
        app.last_focus = app.state.active.and_then(|idx| {
            app.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        Ok(app)
    }

    #[cfg(unix)]
    pub fn unpause_handoff_readers(&self) {
        self.terminal_runtimes.set_handoff_readers_paused(false);
    }

    #[cfg(unix)]
    pub fn assume_handoff_ownership(&mut self) {
        self.terminal_runtimes.assume_handoff_ownership();
    }

    fn request_full_redraw(&mut self) {
        self.full_redraw_pending = true;
    }

    pub(crate) fn sync_prefix_input_source(&mut self, previous_mode: Mode) {
        // Emit the input-source intent on entering/leaving the ASCII realm, like `ClipboardWrite`;
        // the foreground (client, or this app in monolithic mode) applies the switch. Keyed on the
        // realm so multi-level prefix commands stay ASCII. The switch is flag-gated but the restore
        // always fires on exit, so a mid-interaction flag toggle can't strand the host on ASCII.
        let active = match (
            previous_mode.wants_ascii_input(),
            self.state.mode.wants_ascii_input(),
        ) {
            (false, true) if self.state.switch_ascii_input_source_in_prefix => true,
            (true, false) => false,
            _ => return,
        };
        if let Err(err) = self
            .event_tx
            .try_send(crate::events::AppEvent::PrefixInputSource { active })
        {
            tracing::warn!(active, %err, "failed to queue prefix input-source change");
        }
    }

    pub(crate) fn handle_internal_event_with_prefix_sync(
        &mut self,
        event: crate::events::AppEvent,
    ) {
        let previous_mode = self.state.mode;
        self.handle_internal_event(event);
        self.sync_prefix_input_source(previous_mode);
    }

    #[cfg(test)]
    pub(crate) fn set_prefix_input_source(
        &mut self,
        source: Box<dyn crate::platform::PrefixInputSource>,
    ) {
        self.prefix_input_source = source;
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        if self.input_rx.is_none() {
            self.input_rx = Some(crate::raw_input::spawn_input_reader());
        }
        self.query_host_terminal_theme();

        let mut needs_render = true;
        let mut host_mouse_capture_active = self.state.mouse_capture;

        while !self.state.should_quit {
            if self.render_dirty.load(Ordering::Acquire) {
                needs_render = true;
            }

            // Drain a bounded internal-event batch for responsiveness. API handlers
            // perform an exhaustive drain before reading pane/runtime state.
            if self.drain_internal_events() {
                needs_render = true;
            }
            if self.drain_api_requests() {
                needs_render = true;
            }

            self.sync_focus_events();
            self.sync_session_save_schedule();

            let now = Instant::now();
            if self.handle_scheduled_tasks(now, needs_render) {
                needs_render = true;
            }

            if self.state.request_complete_onboarding {
                self.state.request_complete_onboarding = false;
                self.open_settings_from_onboarding();
                needs_render = true;
            }

            if self.state.request_new_workspace {
                self.state.request_new_workspace = false;
                self.runtime_workspace_create(
                    "tui.workspace.create",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: None,
                        focus: true,
                        label: None,
                        env: Default::default(),
                    },
                );
                needs_render = true;
            }

            if self.state.request_new_tab {
                self.state.request_new_tab = false;
                let label = self.state.requested_new_tab_name.take();
                self.runtime_tab_create(
                    "tui.tab.create",
                    crate::api::schema::TabCreateParams {
                        workspace_id: None,
                        cwd: None,
                        focus: true,
                        label,
                        env: Default::default(),
                    },
                );
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_new_linked_worktree.take() {
                self.open_new_linked_worktree_dialog(ws_idx);
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_open_existing_worktree.take() {
                self.open_existing_worktree_dialog(ws_idx);
                needs_render = true;
            }

            if let Some(cwd) = self.state.request_new_workspace_cwd.take() {
                self.runtime_workspace_create(
                    "tui.workspace.create_cwd",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: Some(cwd.display().to_string()),
                        focus: true,
                        label: None,
                        env: Default::default(),
                    },
                );
                needs_render = true;
            }

            if let Some(ws_idx) = self.state.request_remove_linked_worktree.take() {
                self.open_remove_linked_worktree_confirmation(ws_idx);
                needs_render = true;
            }

            if self.state.request_submit_worktree_create {
                self.state.request_submit_worktree_create = false;
                self.submit_worktree_create_via_api();
                needs_render = true;
            }

            if self.state.request_submit_worktree_open {
                self.state.request_submit_worktree_open = false;
                self.submit_worktree_open_via_api();
                needs_render = true;
            }

            if self.state.request_submit_worktree_remove {
                self.state.request_submit_worktree_remove = false;
                self.submit_worktree_remove_via_api();
                needs_render = true;
            }

            if self.state.request_reload_config {
                self.state.request_reload_config = false;
                self.reload_config();
                needs_render = true;
            }

            if self.ensure_default_workspace() {
                needs_render = true;
            }

            let now = Instant::now();
            self.sync_animation_timer(now);
            self.sync_host_mouse_capture(&mut host_mouse_capture_active)?;

            if needs_render && self.can_render_now(now) {
                self.render_dirty.swap(false, Ordering::AcqRel);
                let _sync_output = SyncOutputGuard::begin()?;
                let kitty_graphics_enabled = self.state.kitty_graphics_enabled;
                if self.full_redraw_pending {
                    if kitty_graphics_enabled {
                        crate::kitty_graphics::clear_all_host_graphics()?;
                    }
                    terminal.clear()?;
                    self.full_redraw_pending = false;
                }
                let mut cell_size = crate::kitty_graphics::HostCellSize::default();
                terminal.draw(|frame| {
                    let area = frame.area();
                    if kitty_graphics_enabled {
                        cell_size = crate::kitty_graphics::HostCellSize::from_terminal(area);
                        crate::ui::compute_view_with_cell_size(
                            &mut self.state,
                            &self.terminal_runtimes,
                            area,
                            cell_size,
                        );
                    } else {
                        crate::ui::compute_view_with_runtime_registry(
                            &mut self.state,
                            &self.terminal_runtimes,
                            area,
                        );
                    }
                    crate::ui::render_with_runtime_registry(
                        &self.state,
                        &self.terminal_runtimes,
                        frame,
                    );
                })?;
                if kitty_graphics_enabled {
                    crate::kitty_graphics::paint_local_pane_graphics(
                        &self.state,
                        &self.terminal_runtimes,
                        cell_size,
                    )?;
                }
                self.sync_pending_agent_resume_deadline(now);
                if self.start_pending_agent_resumes(self.pending_agent_resume_due(now)) {
                    self.render_dirty.store(true, Ordering::Release);
                    self.render_notify.notify_one();
                }
                self.last_render_at = Some(now);
                needs_render = false;
                continue;
            }

            let next_deadline = self.next_loop_deadline(now, needs_render);
            let event = {
                let input_rx = self.input_rx.as_mut();
                tokio::select! {
                    maybe_api = self.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(Box::new(msg)),
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
                    self.handle_internal_event_with_prefix_sync(ev);
                    needs_render = true;
                }
                LoopEvent::Api(msg) => {
                    if self.handle_api_request_message(*msg) {
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

    fn sync_host_mouse_capture(&self, active: &mut bool) -> io::Result<()> {
        let desired = self
            .state
            .should_capture_host_mouse_from(&self.terminal_runtimes);
        if desired == *active {
            return Ok(());
        }
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        if desired {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        *active = desired;
        Ok(())
    }

    pub(crate) fn ensure_default_workspace(&mut self) -> bool {
        if !self.state.workspaces.is_empty() || self.state.mode == Mode::Onboarding {
            return false;
        }

        let previous_mode = self.state.mode;
        let preserve_mode = matches!(
            previous_mode,
            Mode::ReleaseNotes | Mode::ProductAnnouncement | Mode::Settings
        );
        let cwd = self.resolve_new_terminal_cwd(None);

        match self.create_workspace_with_options(cwd, true) {
            Ok(_) => {
                if preserve_mode {
                    self.state.mode = previous_mode;
                }
                true
            }
            Err(err) => {
                tracing::error!(err = %err, "failed to create default workspace");
                self.state.mode = Mode::Navigate;
                false
            }
        }
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

        if self.state.product_announcement.is_some() {
            self.state.mode = Mode::ProductAnnouncement;
        } else {
            self.state.mode = if self.state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
    }

    pub(crate) fn dismiss_product_announcement(&mut self) {
        if let Some(announcement) = self.state.product_announcement.take() {
            if !announcement.preview {
                if let Err(err) =
                    crate::product_announcements::mark_seen(&announcement.version, &announcement.id)
                {
                    self.state.config_diagnostic =
                        Some(format!("failed to update announcement status: {err}"));
                    self.config_diagnostic_deadline = Some(Instant::now() + Duration::from_secs(5));
                }
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

    pub(crate) fn scroll_product_announcement(&mut self, delta: i16) {
        let max_scroll = self.state.product_announcement_max_scroll();
        if let Some(announcement) = &mut self.state.product_announcement {
            announcement.scroll = if delta.is_negative() {
                announcement.scroll.saturating_sub(delta.unsigned_abs())
            } else {
                announcement.scroll.saturating_add(delta as u16)
            }
            .min(max_scroll);
        }
    }

    pub(crate) fn open_settings_from_onboarding(&mut self) {
        self.mark_onboarding_complete();
        self.refresh_integration_recommendations();
        crate::app::input::open_settings_at(&mut self.state, state::SettingsSection::Integrations);
    }

    pub(crate) fn refresh_integration_recommendations(&mut self) {
        self.state.integration_recommendations = crate::integration::integration_recommendations();
    }

    pub(crate) fn install_recommended_integrations(&mut self) {
        let targets = self
            .state
            .integration_recommendations
            .iter()
            .filter(|recommendation| recommendation.needs_install())
            .map(|recommendation| recommendation.target)
            .collect::<Vec<_>>();

        self.state.integration_install_messages.clear();
        if targets.is_empty() {
            self.state
                .integration_install_messages
                .push("all detected integrations are current".to_string());
            return;
        }

        for target in targets {
            let label = crate::integration::integration_target_label(target);
            match crate::integration::install_target(target) {
                Ok(messages) => {
                    self.state
                        .integration_install_messages
                        .push(format!("installed {label}"));
                    self.state
                        .integration_install_messages
                        .extend(messages.into_iter().filter(|message| {
                            message.starts_with(crate::integration::INSTALL_WARNING_PREFIX)
                        }));
                }
                Err(err) => self
                    .state
                    .integration_install_messages
                    .push(format!("{label}: {err}")),
            }
        }

        self.state.integration_recommendations = crate::integration::integration_recommendations();
        self.state.mark_session_dirty();
    }

    pub(crate) fn reload_config(&mut self) -> crate::config::ConfigReloadReport {
        self.apply_config_from_disk(true)
    }

    pub(crate) fn take_config_reloaded_from_disk(&mut self) -> bool {
        let reloaded = self.config_reloaded_from_disk;
        self.config_reloaded_from_disk = false;
        reloaded
    }

    pub(crate) fn apply_config_from_disk(
        &mut self,
        notify_success: bool,
    ) -> crate::config::ConfigReloadReport {
        self.config_reloaded_from_disk = true;
        let previous_toast = self.state.toast.clone();
        let report = match crate::config::load_live_config() {
            Ok(loaded) => self.apply_live_config(
                &loaded.config,
                &loaded.diagnostics,
                &loaded.invalid_sections,
                notify_success,
            ),
            Err(diagnostics) => {
                self.state.toast = None;
                self.state.config_diagnostic =
                    crate::config::config_diagnostic_summary(&diagnostics);
                self.config_diagnostic_deadline = None;
                crate::config::ConfigReloadReport {
                    status: crate::config::ConfigReloadStatus::Failed,
                    diagnostics,
                }
            }
        };
        self.sync_toast_deadline(previous_toast);
        report
    }

    fn apply_live_config(
        &mut self,
        config: &crate::config::Config,
        load_diagnostics: &[String],
        invalid_sections: &[String],
        notify_success: bool,
    ) -> crate::config::ConfigReloadReport {
        let mut diagnostics = load_diagnostics.to_vec();
        let invalid_section =
            |section: &str| invalid_sections.iter().any(|invalid| invalid == section);

        if !invalid_section("keys") {
            match config.live_keybinds_with_diagnostics() {
                Ok((live, keybind_diagnostics)) => {
                    self.state.prefix_code = live.prefix.0;
                    self.state.prefix_mods = live.prefix.1;
                    self.state.keybinds = live.keybinds;
                    diagnostics.extend(keybind_diagnostics);
                }
                Err(keybind_diagnostics) => {
                    diagnostics.extend(
                        keybind_diagnostics
                            .into_iter()
                            .map(|diagnostic| format!("{diagnostic}; kept current keybinds")),
                    );
                }
            }
        }

        if !invalid_section("ui") {
            // Validate sidebar bounds before they reach any `u16::clamp` call.
            // On `min > max`, treat the entire `[ui]` section as invalid: keep
            // the previous settings and skip the section so the re-clamp below
            // — and every subsequent render/drag — can never panic.
            if crate::config::validated_sidebar_bounds(
                config.ui.sidebar_min_width,
                config.ui.sidebar_max_width,
            )
            .is_none()
            {
                diagnostics.push(format!(
                    "ui.sidebar_min_width ({}) is greater than sidebar_max_width ({}); keeping previous [ui] settings",
                    config.ui.sidebar_min_width, config.ui.sidebar_max_width,
                ));
            } else {
                diagnostics.extend(config.ui.sound.diagnostics());

                self.state.default_sidebar_width = config.ui.sidebar_width;
                if self.state.sidebar_width_source == state::SidebarWidthSource::ConfigDefault {
                    self.state.sidebar_width = config.ui.sidebar_width;
                }
                self.state.sidebar_min_width = config.ui.sidebar_min_width;
                self.state.sidebar_max_width = config.ui.sidebar_max_width;
                self.state.sidebar_collapsed_mode = config.ui.sidebar_collapsed_mode;
                self.state.mobile_width_threshold = config.ui.mobile_width_threshold;
                // Re-clamp the live width to the new bounds. No source guard — bounds
                // always apply, including to widths owned by Persisted or Manual.
                self.state.sidebar_width = self
                    .state
                    .sidebar_width
                    .clamp(self.state.sidebar_min_width, self.state.sidebar_max_width);
                self.state.mouse_capture = config.ui.mouse_capture;
                if self.state.redraw_on_focus_gained != config.ui.redraw_on_focus_gained {
                    self.state.request_client_config_reload = true;
                }
                self.state.redraw_on_focus_gained = config.ui.redraw_on_focus_gained;
                if self.loaded_host_cursor != config.ui.host_cursor {
                    self.state.request_client_config_reload = true;
                }
                self.loaded_host_cursor = config.ui.host_cursor;
                self.state.mouse_scroll_lines = config.ui.mouse_scroll_lines();
                self.state.right_click_passthrough_modifiers =
                    config.ui.right_click_passthrough_modifiers();
                self.state.confirm_close = config.ui.confirm_close;
                self.state.prompt_new_tab_name = config.ui.prompt_new_tab_name;
                self.state.pane_borders = config.ui.pane_borders;
                self.state.pane_gaps = config.ui.pane_gaps;
                self.state.show_agent_labels_on_pane_borders =
                    config.ui.show_agent_labels_on_pane_borders;
                self.state.hide_tab_bar_when_single_tab = config.ui.hide_tab_bar_when_single_tab;
                self.state.agent_panel_sort =
                    agent_panel_sort_from_config(config.ui.agent_panel_sort);
                self.state.agent_panel_scroll = 0;
                self.state.accent = crate::config::parse_color(&config.ui.accent);
                if !self.state.local_sound_playback && self.state.sound != config.ui.sound {
                    self.state.request_client_config_reload = true;
                }
                self.state.sound = config.ui.sound.clone();
                self.state.toast_config = config.ui.toast.clone();
            }
        }

        if !invalid_section("experimental") {
            let was_kitty_graphics_enabled = self.state.kitty_graphics_enabled;
            self.state.kitty_graphics_enabled = config.experimental.kitty_graphics;
            crate::kitty_graphics::set_enabled(config.experimental.kitty_graphics);
            if was_kitty_graphics_enabled && !config.experimental.kitty_graphics {
                let _ = crate::kitty_graphics::clear_all_host_graphics();
            }
            self.state.reveal_hidden_cursor_for_cjk_ime =
                config.experimental.reveal_hidden_cursor_for_cjk_ime;
            self.state.cjk_ime_agent_filter_configured =
                !config.experimental.cjk_ime_agents.is_empty();
            self.state.cjk_ime_agents = parse_cjk_ime_agents(&config.experimental.cjk_ime_agents);
            self.state.cjk_ime_cursor_shape =
                config.experimental.cjk_ime_cursor_shape.to_decscusr();
            self.state.switch_ascii_input_source_in_prefix =
                config.experimental.switch_ascii_input_source_in_prefix;
            self.persist_pane_history = config.experimental.pane_history;
            self.state.pane_history_persistence = config.experimental.pane_history;
            if !self.persist_pane_history {
                crate::persist::clear_history();
            }
        }

        if !invalid_section("advanced") {
            self.state.pane_scrollback_limit_bytes = config.advanced.scrollback_limit_bytes;
        }

        if !invalid_section("update") {
            let now = Instant::now();
            let previous_version_check_enabled = self.update_version_check_enabled;
            let previous_manifest_check_enabled = self.update_manifest_check_enabled;
            self.update_version_check_enabled = config.update.version_check;
            self.update_manifest_check_enabled = config.update.manifest_check;

            if !self.update_version_check_enabled {
                self.next_auto_update_check = None;
            } else if !previous_version_check_enabled
                && background_update_check_enabled(
                    self.no_session,
                    self.update_version_check_enabled,
                )
                && self.state.update_available.is_none()
            {
                self.next_auto_update_check = Some(now);
            }

            if !self.update_manifest_check_enabled {
                self.next_agent_manifest_update_check = None;
            } else if !previous_manifest_check_enabled
                && background_update_check_enabled(
                    self.no_session,
                    self.update_manifest_check_enabled,
                )
            {
                self.next_agent_manifest_update_check = Some(now);
            }
        }

        if !invalid_section("terminal") {
            self.state.default_shell = config.terminal.default_shell.clone();
            self.state.shell_mode = config.terminal.shell_mode;
            self.state.new_terminal_cwd = config.terminal.new_cwd.clone();
        }

        if !invalid_section("worktrees") {
            self.state.worktree_directory =
                crate::worktree::expand_tilde_absolute_path(&config.worktrees.directory);
        }

        if !invalid_section("theme") {
            self.state.theme_runtime = theme_runtime_config(config, !invalid_section("ui"));
            self.refresh_effective_app_theme();
        }

        let status = if diagnostics.is_empty() {
            crate::config::ConfigReloadStatus::Applied
        } else {
            crate::config::ConfigReloadStatus::Partial
        };

        if diagnostics.is_empty() {
            self.state.config_diagnostic = None;
            self.config_diagnostic_deadline = None;
            if notify_success {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::UpdateInstalled,
                    title: "reloaded config".to_string(),
                    context: "using config.toml".to_string(),
                    position: None,
                    target: None,
                });
            }
        } else {
            self.state.config_diagnostic = crate::config::config_diagnostic_summary(&diagnostics);
            self.config_diagnostic_deadline = None;
            if notify_success {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::UpdateInstalled,
                    title: "reloaded config".to_string(),
                    context: "with warnings".to_string(),
                    position: None,
                    target: None,
                });
            }
        }

        crate::config::ConfigReloadReport {
            status,
            diagnostics,
        }
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
            let previous_mode = self.state.mode;
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
                                self.handle_non_terminal_key_headless(key);
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
                    if self.state.mouse_capture {
                        self.handle_mouse_event_headless(mouse);
                    } else {
                        self.state
                            .handle_pane_mouse_only(&self.terminal_runtimes, mouse);
                    }
                }
                crate::raw_input::RawInputEvent::Paste(text) => {
                    if self.state.mode != Mode::Terminal {
                        self.paste_into_active_text_input(&text);
                    } else {
                        if let Some(ws_idx) = self.state.active {
                            if let Some(ws) = self.state.workspaces.get(ws_idx) {
                                if let Some(focused) = ws.focused_pane_id() {
                                    if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                                        &self.terminal_runtimes,
                                        ws_idx,
                                        focused,
                                    ) {
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
                crate::raw_input::RawInputEvent::OuterFocusGained
                | crate::raw_input::RawInputEvent::OuterFocusLost => {}
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    if apply_host_terminal_theme {
                        self.update_host_terminal_theme(kind, color);
                    }
                }
                crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                    if apply_host_terminal_theme {
                        self.set_host_terminal_appearance(appearance, true);
                    }
                }
                crate::raw_input::RawInputEvent::Unsupported => {}
            }
            self.sync_prefix_input_source(previous_mode);
        }
    }

    /// Handles a key event in non-terminal mode for the headless server.
    ///
    /// Uses the standalone handler functions that work on `&mut AppState`
    /// since the server doesn't have the async context of the monolithic App.
    fn handle_non_terminal_key_headless(&mut self, key: crate::input::TerminalKey) {
        let key_event = key.as_key_event();
        if input::modal_paste_target_active(&self.state)
            && input::is_modal_paste_shortcut(&key_event)
        {
            if let Some(text) = crate::platform::read_clipboard_text() {
                self.paste_into_active_text_input(&text);
            }
            return;
        }

        match self.state.mode {
            Mode::Prefix => {
                self.handle_prefix_key(key);
            }
            Mode::Navigate => {
                self.handle_navigate_key(key);
            }
            Mode::Copy => {
                self.handle_copy_mode_key(key);
            }
            Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane => {
                self.handle_rename_key_via_api(key_event);
            }
            Mode::NewLinkedWorktree => {
                self.handle_worktree_create_key(key_event);
            }
            Mode::OpenExistingWorktree => {
                self.handle_worktree_open_key(key_event);
            }
            Mode::ConfirmRemoveWorktree => {
                self.handle_worktree_remove_key(key_event);
            }
            Mode::Resize => {
                self.handle_resize_key_via_api(key);
            }
            Mode::ConfirmClose => {
                self.handle_confirm_close_key_via_api(key_event);
            }
            Mode::ContextMenu => {
                self.handle_context_menu_key_via_api(key_event);
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
            Mode::ProductAnnouncement => {
                self.handle_product_announcement_key(key_event);
            }
            Mode::Settings => {
                self.handle_settings_key(key_event);
            }
            Mode::Navigator => {
                input::handle_navigator_key(&mut self.state, &self.terminal_runtimes, key_event);
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
    use crate::terminal::TerminalRuntime;
    use crate::workspace::Workspace;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Mutex;

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
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[cfg(windows)]
    fn exiting_test_command() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn exiting_test_command() -> &'static str {
        "/usr/bin/true"
    }

    #[derive(Clone, Default)]
    struct FakePrefixInputSource {
        switch_calls: Rc<Cell<usize>>,
        restore_calls: Rc<Cell<usize>>,
        switched: Rc<Cell<bool>>,
        will_switch: bool,
    }

    impl FakePrefixInputSource {
        fn switching() -> Self {
            Self {
                will_switch: true,
                ..Self::default()
            }
        }

        fn no_op() -> Self {
            Self {
                will_switch: false,
                ..Self::default()
            }
        }
    }

    impl crate::platform::PrefixInputSource for FakePrefixInputSource {
        fn switch_to_ascii(&mut self) {
            self.switch_calls.set(self.switch_calls.get() + 1);
            if self.will_switch {
                self.switched.set(true);
            }
        }

        fn restore(&mut self) {
            if self.switched.replace(false) {
                self.restore_calls.set(self.restore_calls.get() + 1);
            }
        }
    }

    /// Drain the app event channel, returning the `active` flags of any emitted
    /// `PrefixInputSource` events (the host-local input-source intents).
    fn drained_prefix_active(app: &mut App) -> Vec<bool> {
        let mut out = Vec::new();
        while let Ok(ev) = app.event_rx.try_recv() {
            if let crate::events::AppEvent::PrefixInputSource { active } = ev {
                out.push(active);
            }
        }
        out
    }

    #[test]
    fn sync_prefix_input_source_emits_switch_then_restore_when_enabled() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        // Terminal -> Prefix emits the ASCII-switch intent.
        app.state.mode = Mode::Prefix;
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> Terminal emits the restore intent.
        app.state.mode = Mode::Terminal;
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn sync_prefix_input_source_does_not_emit_switch_when_flag_disabled() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = false;

        // Entering the realm with the flag off emits nothing.
        app.state.mode = Mode::Prefix;
        app.sync_prefix_input_source(Mode::Terminal);
        assert!(drained_prefix_active(&mut app).is_empty());

        // Leaving the realm still emits the restore (harmless if nothing was switched), so a
        // mid-interaction flag toggle can't strand the host on ASCII.
        app.state.mode = Mode::Terminal;
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn mode_wants_ascii_input_classification() {
        // Allowlist: the prefix command/navigation realm wants ASCII.
        for mode in [
            Mode::Prefix,
            Mode::Navigate,
            Mode::Navigator,
            Mode::Copy,
            Mode::Resize,
            Mode::ConfirmClose,
            Mode::ConfirmRemoveWorktree,
            Mode::ContextMenu,
            Mode::GlobalMenu,
            Mode::KeybindHelp,
        ] {
            assert!(mode.wants_ascii_input(), "{mode:?} should want ASCII");
        }
        // Everything else (terminal, text entry, startup overlays) keeps the user's IME.
        for mode in [
            Mode::Terminal,
            Mode::RenameWorkspace,
            Mode::RenameTab,
            Mode::RenamePane,
            Mode::NewLinkedWorktree,
            Mode::OpenExistingWorktree,
            Mode::Settings,
            Mode::Onboarding,
            Mode::ReleaseNotes,
            Mode::ProductAnnouncement,
        ] {
            assert!(!mode.wants_ascii_input(), "{mode:?} should keep the IME");
        }
    }

    #[test]
    fn sync_prefix_input_source_keeps_realm_across_multi_level_prefix_commands() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        // Terminal -> Prefix switches once.
        app.state.mode = Mode::Prefix;
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> sub-mode and sub-mode -> sub-mode stay in the realm: no emit.
        app.state.mode = Mode::Navigator;
        app.sync_prefix_input_source(Mode::Prefix);
        app.state.mode = Mode::Resize;
        app.sync_prefix_input_source(Mode::Navigator);
        assert!(
            drained_prefix_active(&mut app).is_empty(),
            "must not switch or restore while still in the realm"
        );

        // Leaving the realm back to the terminal restores.
        app.state.mode = Mode::Terminal;
        app.sync_prefix_input_source(Mode::Resize);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn sync_prefix_input_source_restores_when_entering_rename_text_mode() {
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;

        app.state.mode = Mode::Prefix;
        app.sync_prefix_input_source(Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Prefix -> RenameTab leaves the realm (text entry wants the IME): restore.
        app.state.mode = Mode::RenameTab;
        app.sync_prefix_input_source(Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    #[test]
    fn handle_internal_event_prefix_input_source_applies_switch_and_restore() {
        // The monolithic (in-process) path applies the host switch when it consumes the event.
        let mut app = test_app();
        let fake = FakePrefixInputSource::switching();
        let switch_calls = fake.switch_calls.clone();
        let restore_calls = fake.restore_calls.clone();
        app.set_prefix_input_source(Box::new(fake));

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource { active: true });
        assert_eq!(switch_calls.get(), 1);
        assert_eq!(restore_calls.get(), 0);

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource { active: false });
        assert_eq!(restore_calls.get(), 1);
    }

    #[test]
    fn handle_internal_event_prefix_input_source_restore_is_safe_when_switch_was_noop() {
        // Already-ASCII / failed-switch case: the restore on leave must stay harmless.
        let mut app = test_app();
        let fake = FakePrefixInputSource::no_op();
        let switch_calls = fake.switch_calls.clone();
        let restore_calls = fake.restore_calls.clone();
        app.set_prefix_input_source(Box::new(fake));

        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource { active: true });
        app.handle_internal_event(crate::events::AppEvent::PrefixInputSource { active: false });
        assert_eq!(switch_calls.get(), 1);
        assert_eq!(restore_calls.get(), 0);
    }

    #[tokio::test]
    async fn raw_input_dispatch_emits_input_source_intent_when_leaving_prefix() {
        // Leaving prefix mode happens inside the raw-input dispatch, not in `handle_key` itself —
        // the sync must sit at the dispatch layer so any event that exits prefix (here Esc) still
        // emits the restore intent.
        let mut app = test_app();
        app.state.switch_ascii_input_source_in_prefix = true;
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        // ctrl+b (the default prefix key) enters prefix mode → switch intent.
        app.handle_raw_input_event(raw_key(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        ))
        .await;
        assert_eq!(app.state.mode, Mode::Prefix);
        assert_eq!(drained_prefix_active(&mut app), vec![true]);

        // Esc leaves prefix mode → restore intent.
        app.handle_raw_input_event(raw_key(
            KeyCode::Esc,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ))
        .await;
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(drained_prefix_active(&mut app), vec![false]);
    }

    fn config_env_lock() -> &'static Mutex<()> {
        crate::config::test_config_env_lock()
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

    fn restore_xdg_state_home(original: Option<std::ffi::OsString>) {
        if let Some(value) = original {
            std::env::set_var("XDG_STATE_HOME", value);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    #[test]
    fn git_refresh_deadline_is_suppressed_while_in_flight() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("one"));
        app.git_refresh_in_flight = true;

        assert_eq!(app.git_refresh_deadline(), None);
    }

    #[test]
    fn git_status_event_clears_in_flight_refresh() {
        let mut app = test_app();
        app.git_refresh_in_flight = true;
        let previous_refresh = Instant::now() - Duration::from_secs(10);
        app.last_git_remote_status_refresh = previous_refresh;

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            results: Vec::new(),
            cache_updates: Vec::new(),
        });

        assert!(!app.git_refresh_in_flight);
        assert!(app.last_git_remote_status_refresh > previous_refresh);
    }

    #[test]
    fn git_status_event_marks_render_dirty_when_status_changes() {
        let mut app = test_app();
        app.state.workspaces.push(Workspace::test_new("one"));
        app.render_dirty.store(false, Ordering::Release);
        let workspace_id = app.state.workspaces[0].id.clone();
        let resolved_identity_cwd = app.state.workspaces[0].resolved_identity_cwd().unwrap();

        app.handle_internal_event(AppEvent::GitStatusRefreshed {
            results: vec![crate::workspace::WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd,
                branch: Some("render-dirty-test".into()),
                ahead_behind: Some((1, 0)),
                space: None,
            }],
            cache_updates: Vec::new(),
        });

        assert!(app.render_dirty.load(Ordering::Acquire));
    }

    #[test]
    fn clipboard_write_event_shows_feedback_toast() {
        let mut app = test_app();

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert!(app.state.toast.is_none());
        let feedback = app.state.copy_feedback.as_ref().expect("copy feedback");
        assert_eq!(feedback.message, "copied to clipboard");
        assert!(app.copy_feedback_deadline.is_some());
    }

    #[test]
    fn clipboard_feedback_can_be_disabled() {
        let mut app = test_app();
        app.state.toast_config.clipboard.enabled = false;

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert!(app.state.copy_feedback.is_none());
        assert!(app.copy_feedback_deadline.is_none());
    }

    #[test]
    fn clipboard_feedback_does_not_replace_notification_toast() {
        let mut app = test_app();
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_string(),
            context: "background · 2".to_string(),
            position: None,
            target: None,
        });
        let original_toast = app.state.toast.clone();

        app.handle_internal_event(AppEvent::ClipboardWrite {
            content: b"copied".to_vec(),
        });

        assert_eq!(app.state.toast, original_toast);
        assert_eq!(
            app.state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied to clipboard")
        );
    }

    #[test]
    fn notification_show_api_creates_herdr_toast_with_position() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: Some("api workspace".into()),
                        position: Some(crate::config::ToastHerdrPosition::TopLeft),
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: crate::api::schema::NotificationShowReason::Shown,
            }
        );
        let toast = app.state.toast.as_ref().expect("api toast");
        assert_eq!(toast.title, "build failed");
        assert_eq!(toast.context, "api workspace");
        assert_eq!(
            toast.position,
            Some(crate::config::ToastHerdrPosition::TopLeft)
        );
        assert!(app.toast_deadline.is_some());
    }

    #[test]
    fn notification_show_api_herdr_toast_expires() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: crate::api::schema::NotificationShowReason::Shown,
            }
        );
        let deadline = app.toast_deadline.expect("api toast deadline");
        assert!(app.handle_scheduled_tasks(deadline, false));
        assert!(app.state.toast.is_none());
        assert!(app.toast_deadline.is_none());
    }

    #[test]
    fn notification_show_api_respects_off_delivery() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Off;

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::Disabled,
            }
        );
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn notification_show_api_does_not_replace_existing_toast() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_string(),
            context: "background · 2".to_string(),
            position: None,
            target: None,
        });

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::Busy,
            }
        );
        assert_eq!(
            app.state.toast.as_ref().map(|toast| toast.title.as_str()),
            Some("pi needs attention")
        );
    }

    #[test]
    fn notification_show_api_is_rate_limited() {
        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.mark_api_notification_shown(Instant::now());

        let response =
            app.handle_api_request_after_internal_events_drained(crate::api::schema::Request {
                id: "notify".into(),
                method: crate::api::schema::Method::NotificationShow(
                    crate::api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: crate::api::schema::NotificationShowSound::None,
                    },
                ),
            });

        let parsed: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            crate::api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: crate::api::schema::NotificationShowReason::RateLimited,
            }
        );
        assert!(app.state.toast.is_none());
    }

    #[test]
    fn internal_event_drain_limits_work_per_tick() {
        let mut app = test_app();
        for i in 0..=APP_EVENT_DRAIN_LIMIT {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("2.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        assert!(app.drain_internal_events());

        let expected_version = format!("2.0.{}", APP_EVENT_DRAIN_LIMIT - 1);
        assert_eq!(
            app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(app.event_rx.try_recv().is_ok());
    }

    #[test]
    fn api_request_drains_all_pending_internal_events_before_reading_state() {
        let mut app = test_app();
        for i in 0..=APP_EVENT_DRAIN_LIMIT {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("3.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_server_stop_after_events".into(),
            method: crate::api::schema::Method::ServerStop(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        let expected_version = format!("3.0.{APP_EVENT_DRAIN_LIMIT}");
        assert_eq!(
            app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn startup_uses_configured_agent_panel_sort() {
        let mut config = Config::default();
        config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
    }

    #[test]
    fn startup_uses_redraw_on_focus_gained_config() {
        let mut config = Config::default();
        config.ui.redraw_on_focus_gained = false;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(!app.state.redraw_on_focus_gained);
    }

    #[test]
    fn theme_auto_switch_is_opt_in_and_preserves_manual_default() {
        let mut config = Config::default();
        config.theme.name = Some("tokyo-night".to_string());
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert!(!app.state.theme_runtime.auto_switch);
        assert_eq!(app.state.theme_name, "tokyo-night");
        assert_eq!(app.state.palette, state::Palette::tokyo_night());
    }

    #[test]
    fn theme_auto_switch_uses_sibling_map_and_explicit_appearance() {
        let mut config = Config::default();
        config.theme.name = Some("tokyo-night".to_string());
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.theme_name, "tokyo-night");
        assert!(
            app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true)
        );

        assert_eq!(app.state.theme_name, "tokyo-night-day");
        assert_eq!(app.state.palette, state::Palette::tokyo_night_day());
    }

    #[test]
    fn theme_auto_switch_applies_custom_overrides_after_active_base() {
        let mut config = Config::default();
        config.theme.name = Some("gruvbox".to_string());
        config.theme.auto_switch = true;
        config.theme.custom = Some(crate::config::CustomThemeColors {
            accent: Some("#010203".to_string()),
            ..Default::default()
        });
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Light, true);

        assert_eq!(app.state.theme_name, "gruvbox-light");
        assert_eq!(
            app.state.palette.accent,
            ratatui::style::Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn inferred_background_appearance_does_not_override_explicit_report() {
        let mut config = Config::default();
        config.theme.name = Some("catppuccin".to_string());
        config.theme.auto_switch = true;
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        app.set_host_terminal_appearance(crate::terminal_theme::HostAppearance::Dark, true);
        app.update_host_terminal_theme(
            crate::terminal_theme::DefaultColorKind::Background,
            crate::terminal_theme::RgbColor {
                r: 0xff,
                g: 0xff,
                b: 0xff,
            },
        );

        assert_eq!(
            app.state.host_terminal_appearance,
            Some(crate::terminal_theme::HostAppearance::Dark)
        );
        assert_eq!(app.state.theme_name, "catppuccin");
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
    fn startup_keeps_pending_release_notes_available_without_auto_opening() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("startup-pending-release-notes-no-auto-open");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- One")
            .unwrap();
        let config = Config {
            onboarding: Some(false),
            ..Default::default()
        };
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.release_notes.is_none());
        assert!(app.state.latest_release_notes_available);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn startup_still_auto_opens_unseen_product_announcement() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("startup-product-announcement-auto-open");
        let state_home = path.parent().unwrap().join("state");
        let original_xdg_state_home = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        std::env::set_var("XDG_STATE_HOME", &state_home);

        crate::release_notes::save_pending(env!("CARGO_PKG_VERSION"), "### Changed\n- One")
            .unwrap();
        crate::product_announcements::save_manifest_announcement(
            env!("CARGO_PKG_VERSION"),
            Some(&crate::product_announcements::ManifestAnnouncement {
                id: "startup-announcement".into(),
                title: Some("Startup announcement".into()),
                body: "### Announcement\n- One".into(),
            }),
        )
        .unwrap();

        let config = Config {
            onboarding: Some(false),
            ..Default::default()
        };
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(app.state.mode, Mode::ProductAnnouncement);
        assert_eq!(
            app.state
                .product_announcement
                .as_ref()
                .map(|announcement| announcement.id.as_str()),
            Some("startup-announcement")
        );
        assert!(app.state.release_notes.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        restore_xdg_state_home(original_xdg_state_home);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_live_state() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-success");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[terminal]\ndefault_shell = \"nu\"\nshell_mode = \"non_login\"\nnew_cwd = \"home\"\n[keys]\nnew_workspace = \"prefix+m\"\nprefix = \"ctrl+a\"\n[update]\nversion_check = false\nmanifest_check = false\n[ui]\nagent_panel_scope = \"current\"\nagent_panel_sort = \"priority\"\nredraw_on_focus_gained = false\nright_click_passthrough_modifier = \"ctrl\"\n[ui.toast]\ndelivery = \"herdr\"\n[experimental]\nswitch_ascii_input_source_in_prefix = true\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.next_auto_update_check = Some(Instant::now());
        app.next_agent_manifest_update_check = Some(Instant::now());
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.prefix_code, KeyCode::Char('a'));
        assert_eq!(app.state.prefix_mods, KeyModifiers::CONTROL);
        assert!(app
            .state
            .keybinds
            .new_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        );
        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
        assert!(!app.state.redraw_on_focus_gained);
        assert_eq!(
            app.state.right_click_passthrough_modifiers,
            Some(KeyModifiers::CONTROL)
        );
        assert!(app.state.request_client_config_reload);
        assert_eq!(app.state.default_shell, "nu");
        assert_eq!(
            app.state.shell_mode,
            crate::config::ShellModeConfig::NonLogin
        );
        assert_eq!(
            app.state.new_terminal_cwd,
            crate::config::NewTerminalCwdConfig::Home
        );
        assert!(!app.update_version_check_enabled);
        assert!(!app.update_manifest_check_enabled);
        assert!(app.next_auto_update_check.is_none());
        assert!(app.next_agent_manifest_update_check.is_none());
        assert!(app.state.switch_ascii_input_source_in_prefix);
        assert!(app.state.config_diagnostic.is_none());
        let toast = app.state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, crate::app::state::ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "reloaded config");
        assert_eq!(toast.context, "using config.toml");

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_requests_client_reload_for_host_cursor_only_change() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-host-cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[ui]\nhost_cursor = \"native\"\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.request_client_config_reload = false;

        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(
            app.loaded_host_cursor,
            crate::config::HostCursorModeConfig::Native
        );
        assert!(app.state.request_client_config_reload);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_width_only_when_config_owned() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-sidebar-width");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.sidebar_width_source,
            state::SidebarWidthSource::ConfigDefault
        );

        std::fs::write(&path, "[ui]\nsidebar_width = 34\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.default_sidebar_width, 34);
        assert_eq!(app.state.sidebar_width, 34);

        app.state.sidebar_width = 31;
        app.state.sidebar_width_source = state::SidebarWidthSource::Manual;
        std::fs::write(&path, "[ui]\nsidebar_width = 35\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.default_sidebar_width, 35);
        assert_eq!(app.state.sidebar_width, 31);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_collapsed_mode() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-sidebar-collapsed-mode");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.sidebar_collapsed_mode,
            crate::config::SidebarCollapsedModeConfig::Compact
        );

        std::fs::write(&path, "[ui]\nsidebar_collapsed_mode = \"hidden\"\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(
            app.state.sidebar_collapsed_mode,
            crate::config::SidebarCollapsedModeConfig::Hidden
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_sidebar_bounds_and_reclamps() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-sidebar-bounds");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        // Default bounds.
        assert_eq!(app.state.sidebar_min_width, 18);
        assert_eq!(app.state.sidebar_max_width, 36);
        assert_eq!(
            app.state.mobile_width_threshold,
            crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        // Manually set a width and flip the source so the existing
        // sidebar_width-only-when-config-owned guard does NOT update it.
        app.state.sidebar_width = 30;
        app.state.sidebar_width_source = state::SidebarWidthSource::Manual;

        // Tightening max below the current width must re-clamp the live width
        // even when source is Manual — bounds always apply.
        std::fs::write(&path, "[ui]\nsidebar_max_width = 24\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_max_width, 24);
        assert_eq!(
            app.state.sidebar_width, 24,
            "manual width must re-clamp to new max"
        );

        // Loosening max leaves the live width alone (it's already within bounds).
        app.state.sidebar_width = 24;
        std::fs::write(&path, "[ui]\nsidebar_max_width = 60\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_max_width, 60);
        assert_eq!(app.state.sidebar_width, 24);

        // Raising min above the current width re-clamps upward.
        std::fs::write(&path, "[ui]\nsidebar_min_width = 30\n").unwrap();
        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.sidebar_min_width, 30);
        assert_eq!(
            app.state.sidebar_width, 30,
            "manual width must re-clamp up to new min"
        );

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_updates_mobile_width_threshold() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-mobile-width-threshold");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.mobile_width_threshold,
            crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD
        );

        std::fs::write(&path, "[ui]\nmobile_width_threshold = 96\n").unwrap();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.mobile_width_threshold, 96);

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn app_new_falls_back_to_default_bounds_on_inverted_config() {
        let mut config = Config::default();
        config.ui.sidebar_min_width = 50;
        config.ui.sidebar_max_width = 30;

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(&config, true, None, api_rx, crate::api::EventHub::default());

        assert_eq!(
            app.state.sidebar_min_width, 18,
            "App::new must fall back to default min when bounds are inverted"
        );
        assert_eq!(
            app.state.sidebar_max_width, 36,
            "App::new must fall back to default max when bounds are inverted"
        );
    }

    #[test]
    fn reload_config_invalid_sidebar_bounds_keeps_previous_ui_and_returns_partial() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-invalid-sidebar-bounds");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_min = app.state.sidebar_min_width;
        let original_max = app.state.sidebar_max_width;
        let original_mouse_capture = app.state.mouse_capture;
        // Pair the bad bounds with another `[ui]` field change to confirm the
        // entire section is treated as invalid (not just the bounds).
        let target_mouse_capture = !original_mouse_capture;
        std::fs::write(
            &path,
            format!(
                "[ui]\nsidebar_min_width = 50\nsidebar_max_width = 30\nmouse_capture = {}\n",
                target_mouse_capture
            ),
        )
        .unwrap();

        let report = app.reload_config();
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert_eq!(app.state.sidebar_min_width, original_min);
        assert_eq!(app.state.sidebar_max_width, original_max);
        assert_eq!(
            app.state.mouse_capture, original_mouse_capture,
            "[ui] is treated as invalid on bad bounds; mouse_capture must not apply"
        );
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| {
                message.contains("sidebar_min_width")
                    && message.contains("sidebar_max_width")
                    && message.contains("greater")
            }));

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_disables_invalid_binding_but_applies_valid_keymap_and_other_sections() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-invalid-keybind");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nnew_workspace = \"wat\"\n[ui.toast]\ndelivery = \"terminal\"\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_prefix = (app.state.prefix_code, app.state.prefix_mods);
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert_eq!(
            (app.state.prefix_code, app.state.prefix_mods),
            original_prefix
        );
        assert!(app.state.keybinds.new_workspace.bindings.is_empty());
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| {
                message.contains("keys.new_workspace") && message.contains("disabling binding")
            }));

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_user_binding_displaces_default_without_rejecting_prefix() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-user-binding-displaces-default");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nprefix = \"ctrl+space\"\nprevious_workspace = \"prefix+shift+l\"\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Applied);
        assert_eq!(app.state.prefix_code, KeyCode::Char(' '));
        assert_eq!(app.state.prefix_mods, KeyModifiers::CONTROL);
        assert!(app
            .state
            .keybinds
            .previous_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::SHIFT)));
        assert!(app.state.keybinds.swap_pane_right.bindings.is_empty());
        assert!(app.state.config_diagnostic.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_preserves_invalid_ui_section_but_applies_valid_keys() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-invalid-ui-section");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[keys]\nnew_workspace = \"prefix+m\"\n[ui.toast]\ndelivery = \"desktop\"\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(app
            .state
            .keybinds
            .new_workspace
            .matches_prefix(&KeyEvent::new(KeyCode::Char('m'), KeyModifiers::empty())));
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        );
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("invalid ui config")));

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_preserves_invalid_terminal_section_but_applies_valid_ui() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-invalid-terminal-section");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[terminal]\ndefault_shell = \"nu\"\nshell_mode = \"sideways\"\nnew_cwd = \"home\"\n[ui.toast]\ndelivery = \"terminal\"\n",
        )
        .unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_default_shell = app.state.default_shell.clone();
        let original_shell_mode = app.state.shell_mode;
        let original_new_cwd = app.state.new_terminal_cwd.clone();
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert_eq!(app.state.default_shell, original_default_shell);
        assert_eq!(app.state.shell_mode, original_shell_mode);
        assert_eq!(app.state.new_terminal_cwd, original_new_cwd);
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("invalid terminal config")));

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn settings_save_toast_delivery_persists_then_applies_live_config() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("settings-save-toast-delivery");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Off
        );

        app.save_toast_delivery(crate::config::ToastDelivery::Terminal);

        assert_eq!(
            app.state.toast_config.delivery,
            crate::config::ToastDelivery::Terminal
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("delivery = \"terminal\""));
        assert!(app.state.config_diagnostic.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn save_agent_panel_sort_persists_then_applies_live_config() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("save-agent-panel-sort");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Spaces);

        app.save_agent_panel_sort(state::AgentPanelSort::Priority);

        assert_eq!(app.state.agent_panel_sort, state::AgentPanelSort::Priority);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("agent_panel_sort = \"priority\""));
        assert!(app.state.config_diagnostic.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn settings_save_pane_history_persists_then_applies_live_config() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("settings-save-pane-history");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "onboarding = false\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        assert!(!app.persist_pane_history);
        assert!(!app.state.pane_history_persistence);

        app.save_pane_history_persistence(true);

        assert!(app.persist_pane_history);
        assert!(app.state.pane_history_persistence);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[experimental]"));
        assert!(content.contains("pane_history = true"));
        assert!(app.state.config_diagnostic.is_none());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reload_config_keeps_current_state_on_invalid_toml() {
        let _guard = config_env_lock().lock().unwrap();
        let path = temp_config_path("reload-config-invalid-toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[keys\nnew_workspace = \"g\"\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut app = test_app();
        let original_prefix = (app.state.prefix_code, app.state.prefix_mods);
        let original_keybinds = app.state.keybinds.new_workspace.clone();
        let original_toast_delivery = app.state.toast_config.delivery;
        let report = app.reload_config();

        assert_eq!(report.status, crate::config::ConfigReloadStatus::Failed);
        assert_eq!(
            (app.state.prefix_code, app.state.prefix_mods),
            original_prefix
        );
        assert_eq!(app.state.keybinds.new_workspace, original_keybinds);
        assert_eq!(app.state.toast_config.delivery, original_toast_delivery);
        assert!(app
            .state
            .config_diagnostic
            .as_deref()
            .is_some_and(|message| {
                message.contains("config parse error") && message.contains("keeping current config")
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
    async fn outer_focus_gained_marks_visible_done_panes_seen() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        let split_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let background_tab = workspace.test_add_tab(Some("background"));
        let background_pane = workspace.tabs[background_tab].root_pane;

        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let root_terminal_id = app.state.workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&root_terminal_id)
            .unwrap()
            .state = AgentState::Idle;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&root_pane)
            .unwrap()
            .seen = false;
        let split_terminal_id = app.state.workspaces[0].tabs[0].panes[&split_pane]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&split_terminal_id)
            .unwrap()
            .state = AgentState::Idle;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&split_pane)
            .unwrap()
            .seen = false;
        let bg_terminal_id = app.state.workspaces[0].tabs[background_tab].panes[&background_pane]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Idle;
        app.state.workspaces[0].tabs[background_tab]
            .panes
            .get_mut(&background_pane)
            .unwrap()
            .seen = false;

        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.outer_terminal_focus = Some(false);

        let handled = app
            .handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
            .await;

        assert!(handled);
        assert_eq!(app.state.outer_terminal_focus, Some(true));
        assert!(app.state.workspaces[0].tabs[0].panes[&root_pane].seen);
        assert!(app.state.workspaces[0].tabs[0].panes[&split_pane].seen);
        assert!(!app.state.workspaces[0].tabs[background_tab].panes[&background_pane].seen);
    }

    #[tokio::test]
    async fn outer_focus_gained_does_not_require_full_redraw_when_disabled() {
        let mut app = test_app();
        app.state.redraw_on_focus_gained = false;

        let handled = app
            .handle_raw_input_event(crate::raw_input::RawInputEvent::OuterFocusGained)
            .await;

        assert!(handled);
        assert_eq!(app.state.outer_terminal_focus, Some(true));
        assert!(!app.full_redraw_pending);
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
                    workspace_id: "w1".into(),
                },
            ),
        };
        let pane_rename = crate::api::schema::Request {
            id: "req_3".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: "w1:p1".into(),
                label: Some("logs".into()),
            }),
        };
        let worktree_list = crate::api::schema::Request {
            id: "req_4".into(),
            method: crate::api::schema::Method::WorktreeList(
                crate::api::schema::WorktreeListParams::default(),
            ),
        };
        let worktree_create = crate::api::schema::Request {
            id: "req_5".into(),
            method: crate::api::schema::Method::WorktreeCreate(
                crate::api::schema::WorktreeCreateParams::default(),
            ),
        };
        let pane_swap = crate::api::schema::Request {
            id: "req_6".into(),
            method: crate::api::schema::Method::PaneSwap(crate::api::schema::PaneSwapParams {
                pane_id: Some("w1:p1".into()),
                direction: Some(crate::api::schema::PaneDirection::Right),
                ..crate::api::schema::PaneSwapParams::default()
            }),
        };
        let pane_focus_direction = crate::api::schema::Request {
            id: "req_7".into(),
            method: crate::api::schema::Method::PaneFocusDirection(
                crate::api::schema::PaneFocusDirectionParams {
                    pane_id: Some("w1:p1".into()),
                    direction: crate::api::schema::PaneDirection::Right,
                },
            ),
        };
        let pane_resize = crate::api::schema::Request {
            id: "req_8".into(),
            method: crate::api::schema::Method::PaneResize(crate::api::schema::PaneResizeParams {
                pane_id: Some("w1:p1".into()),
                direction: crate::api::schema::PaneDirection::Right,
                amount: Some(0.05),
            }),
        };

        assert!(!crate::api::request_changes_ui(&read_only));
        assert!(!crate::api::request_changes_ui(&worktree_list));
        assert!(crate::api::request_changes_ui(&mutating));
        assert!(crate::api::request_changes_ui(&pane_rename));
        assert!(crate::api::request_changes_ui(&worktree_create));
        assert!(crate::api::request_changes_ui(&pane_swap));
        assert!(crate::api::request_changes_ui(&pane_focus_direction));
        assert!(crate::api::request_changes_ui(&pane_resize));
    }

    #[test]
    fn workspace_create_response_includes_initial_tab_and_root_pane() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("api-root-pane")];
        app.state.ensure_test_terminals();
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
        assert!(root_pane.terminal_id.starts_with("term_"));
        assert_ne!(root_pane.terminal_id, root_pane.pane_id);
    }

    #[test]
    fn tab_create_response_includes_root_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-tab-root-pane");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
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
    fn tab_info_number_uses_stable_public_tab_number() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-tab-public-number");
        let removed_tab = workspace.test_add_tab(None);
        let survivor_tab = workspace.test_add_tab(None);
        let survivor_pane = workspace.tabs[survivor_tab].root_pane;
        assert!(workspace.close_tab(removed_tab));
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let survivor_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(survivor_pane)
            .unwrap();

        let tab = app.tab_info(0, survivor_idx).unwrap();

        assert_eq!(tab.tab_id, format!("{}:t3", app.state.workspaces[0].id));
        assert_eq!(tab.number, 3);
        assert_eq!(tab.label, "2");
    }

    #[test]
    fn legacy_bare_tab_id_uses_tab_position_not_public_tab_number() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("legacy-tab-id");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        let public_four_tab = workspace.test_add_tab(None);
        let fourth_position_tab = workspace.test_add_tab(None);
        let public_four_pane = workspace.tabs[public_four_tab].root_pane;
        let fourth_position_pane = workspace.tabs[fourth_position_tab].root_pane;
        assert!(workspace.close_tab(removed_tab));
        app.state.workspaces = vec![workspace];

        let public_four_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(public_four_pane)
            .unwrap();
        let fourth_position_idx = app.state.workspaces[0]
            .find_tab_index_for_pane(fourth_position_pane)
            .unwrap();

        assert_eq!(app.state.workspaces[0].tabs[public_four_idx].number, 4);
        assert_eq!(app.state.workspaces[0].tabs[fourth_position_idx].number, 5);
        assert_eq!(
            app.parse_tab_id(&format!("{}:t4", app.state.workspaces[0].id)),
            Some((0, public_four_idx))
        );
        assert_eq!(
            app.parse_tab_id(&format!("{}:4", app.state.workspaces[0].id)),
            Some((0, fourth_position_idx))
        );
    }

    #[test]
    fn workspace_creation_in_navigate_mode_uses_selected_workspace_seed_cwd() {
        let mut app = test_app();
        let mut first = Workspace::test_new("herdr");
        first.identity_cwd = std::path::PathBuf::from("/tmp/herdr");
        let mut second = Workspace::test_new("pion");
        second.identity_cwd = std::path::PathBuf::from("/tmp/pion");

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
    fn new_terminal_cwd_follow_uses_source_cwd() {
        let cwd = creation::resolve_new_terminal_cwd(
            &crate::config::NewTerminalCwdConfig::Follow,
            Some(std::path::PathBuf::from("/tmp/herdr-source")),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/herdr-source"));
    }

    #[test]
    fn new_terminal_cwd_follow_without_source_uses_home() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };

        let cwd =
            creation::resolve_new_terminal_cwd(&crate::config::NewTerminalCwdConfig::Follow, None);

        assert_eq!(cwd, home);
    }

    #[test]
    fn new_terminal_cwd_path_uses_configured_path() {
        let cwd = creation::resolve_new_terminal_cwd(
            &crate::config::NewTerminalCwdConfig::Path("/tmp/herdr-fixed".into()),
            Some(std::path::PathBuf::from("/tmp/herdr-source")),
        );

        assert_eq!(cwd, std::path::PathBuf::from("/tmp/herdr-fixed"));
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
    fn pane_rename_request_sets_and_clears_manual_label() {
        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-rename");
        let pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let pane_id = app.pane_info(0, pane).unwrap().pane_id;
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_rename".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("reviewer".into()),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["label"], "reviewer");
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .manual_label
                .as_deref(),
            Some("reviewer")
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_rename_clear".into(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id,
                label: None,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert!(response["result"]["pane"].get("label").is_none());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .unwrap()
            .manual_label
            .is_none());
    }

    #[test]
    fn terminal_target_resolves_terminal_id() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-id");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        let resolved = app.resolve_terminal_target(&terminal_id).unwrap();

        assert_eq!(resolved.ws_idx, 0);
        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);
    }

    #[test]
    fn terminal_target_resolves_legacy_pane_id() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-pane");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.public_pane_id(0, pane).unwrap();

        let resolved = app.resolve_terminal_target(&pane_id).unwrap();

        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);
    }

    #[test]
    fn terminal_target_resolves_unique_agent_name() {
        let mut app = test_app();
        let workspace = Workspace::test_new("terminal-target-name");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().to_string();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let attached_terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&attached_terminal_id)
            .unwrap()
            .set_agent_name("reviewer".into());
        app.state.active = Some(0);
        app.state.selected = 0;

        let resolved = app.resolve_terminal_target("reviewer").unwrap();

        assert_eq!(resolved.pane_id, pane);
        assert_eq!(resolved.terminal_id, terminal_id);
    }

    #[test]
    fn terminal_target_reports_missing_target() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("terminal-target-missing")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let err = app.resolve_terminal_target("missing-agent").unwrap_err();

        assert_eq!(
            err,
            crate::app::terminal_targets::TerminalTargetError::NotFound {
                target: "missing-agent".into()
            }
        );
    }

    #[test]
    fn terminal_target_reports_ambiguous_duplicate_agent_name() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("terminal-target-ambiguous");
        let first = workspace.tabs[0].root_pane;
        let second = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0]
            .pane_state(first)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("worker".into());
        let second_terminal_id = app.state.workspaces[0]
            .pane_state(second)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .set_agent_name("worker".into());
        app.state.active = Some(0);
        app.state.selected = 0;

        let err = app.resolve_terminal_target("worker").unwrap_err();

        let crate::app::terminal_targets::TerminalTargetError::Ambiguous { target, candidates } =
            err
        else {
            panic!("expected ambiguous terminal target");
        };
        assert_eq!(target, "worker");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| {
            candidate.terminal_id.starts_with("term_")
                && candidate.pane_id.starts_with(&app.state.workspaces[0].id)
                && candidate.workspace_id == app.state.workspaces[0].id
                && candidate.cwd.is_some()
        }));
    }

    #[tokio::test]
    async fn pane_split_request_targets_pane_in_background_tab() {
        let _guard = config_env_lock().lock().unwrap();
        let original_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", exiting_test_command());

        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-split-background-tab");
        let active_pane = workspace.tabs[0].root_pane;
        let background_tab = workspace.test_add_tab(Some("worker"));
        let target_pane = workspace.tabs[background_tab].root_pane;
        workspace.switch_tab(background_tab);
        let background_previous_focus =
            workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.switch_tab(0);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        let split_cwd = std::env::temp_dir();
        let target_terminal_id = app.state.workspaces[0]
            .pane_state(target_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&target_terminal_id)
            .unwrap()
            .cwd = split_cwd.clone();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state
            .focus_pane_in_workspace(0, background_previous_focus);
        app.state.focus_pane_in_workspace(0, active_pane);

        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;
        let target_tab_id = app.public_tab_id(0, background_tab).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_background_tab".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(target_pane_id),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                env: Default::default(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["tab_id"], target_tab_id);
        let response_cwd =
            std::path::PathBuf::from(response["result"]["pane"]["cwd"].as_str().unwrap());
        assert_eq!(
            crate::worktree::canonical_or_original(&response_cwd),
            crate::worktree::canonical_or_original(&split_cwd)
        );
        assert_eq!(response["result"]["pane"]["focused"], false);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            active_pane
        );
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .focused(),
            background_previous_focus
        );
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .pane_count(),
            3
        );
        app.state.last_pane();
        assert_eq!(app.state.workspaces[0].active_tab, background_tab);
        assert_eq!(
            app.state.workspaces[0].tabs[background_tab]
                .layout
                .focused(),
            background_previous_focus
        );

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => std::env::set_var("SHELL", value),
            None => std::env::remove_var("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_request_focuses_new_pane_when_requested() {
        let _guard = config_env_lock().lock().unwrap();
        let original_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", exiting_test_command());

        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-split-focus-background-tab");
        let background_tab = workspace.test_add_tab(Some("worker"));
        workspace.switch_tab(0);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane = app.state.workspaces[0].tabs[background_tab].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;
        let target_tab_id = app.public_tab_id(0, background_tab).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_focus_background_tab".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(target_pane_id),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: true,
                env: Default::default(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(response["result"]["pane"]["tab_id"], target_tab_id);
        assert_eq!(response["result"]["pane"]["focused"], true);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, background_tab);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => std::env::set_var("SHELL", value),
            None => std::env::remove_var("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_request_applies_ratio() {
        let _guard = config_env_lock().lock().unwrap();
        let original_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/usr/bin/true");

        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-split-ratio");
        let target_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_ratio".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: Some(target_pane_id),
                direction: crate::api::schema::SplitDirection::Right,
                ratio: Some(0.333),
                cwd: None,
                focus: false,
                env: Default::default(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        let splits = app.state.workspaces[0].tabs[0]
            .layout
            .splits(ratatui::layout::Rect::new(0, 0, 100, 20));
        assert_eq!(splits.len(), 1);
        assert!((splits[0].ratio - 0.333).abs() < f32::EPSILON);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => std::env::set_var("SHELL", value),
            None => std::env::remove_var("SHELL"),
        }
    }

    #[tokio::test]
    async fn pane_split_request_uses_active_focused_pane_when_target_is_omitted() {
        let _guard = config_env_lock().lock().unwrap();
        let original_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/usr/bin/true");

        let mut app = test_app();
        let workspace = Workspace::test_new("api-pane-split-current");
        let target_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.focus_pane_in_workspace(0, target_pane);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_split_current".into(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction: crate::api::schema::SplitDirection::Right,
                ratio: None,
                cwd: None,
                focus: false,
                env: Default::default(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "pane_info");
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            target_pane
        );

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
        match original_shell {
            Some(value) => std::env::set_var("SHELL", value),
            None => std::env::remove_var("SHELL"),
        }
    }

    #[tokio::test]
    async fn focused_agent_start_records_previous_pane() {
        let mut app = test_app();
        let workspace = Workspace::test_new("agent-start-focus");
        let root = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_agent_start_focus".into(),
            method: crate::api::schema::Method::AgentStart(crate::api::schema::AgentStartParams {
                name: "worker".into(),
                cwd: None,
                workspace_id: None,
                tab_id: None,
                split: Some(crate::api::schema::SplitDirection::Right),
                focus: true,
                argv: vec![exiting_test_command().into()],
                env: Default::default(),
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "agent_started");
        assert_ne!(app.state.workspaces[0].focused_pane_id(), Some(root));

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[test]
    fn pane_close_request_closes_only_the_target_tab_when_other_tabs_exist() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("api-pane-close");
        let second_tab = workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(second_tab);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
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
        app.state.ensure_test_terminals();
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
    fn pane_close_request_requires_confirmation_before_closing_parent_worktree_group() {
        let mut app = test_app();
        let mut parent = Workspace::test_new("api-pane-close-parent");
        parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        let mut child = Workspace::test_new("api-pane-close-child");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-child".into(),
            is_linked_worktree: true,
        });
        app.state.workspaces = vec![parent, child];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 1;

        let target_pane = app.state.workspaces[0].tabs[0].root_pane;
        let target_pane_id = app.pane_info(0, target_pane).unwrap().pane_id;

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_pane_close_parent_group".into(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: target_pane_id,
            }),
        });
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["error"]["code"], "confirmation_required");
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.workspaces.len(), 2);
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
            app.next_headless_loop_deadline_with_git_refresh(now, false, true),
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

        assert_eq!(
            app.next_headless_loop_deadline_with_git_refresh(now, false, true),
            None
        );
    }

    #[test]
    fn due_session_save_deadline_is_cleared() {
        let mut app = test_app();
        app.session_save_deadline = Some(Instant::now() - Duration::from_secs(1));

        app.handle_scheduled_tasks(Instant::now(), false);

        assert!(app.session_save_deadline.is_none());
    }

    #[test]
    fn next_loop_deadline_includes_selection_autoscroll_deadline() {
        let mut app = test_app();
        let now = Instant::now();
        app.next_resize_poll = now + Duration::from_millis(300);
        app.selection_autoscroll_deadline = Some(now + Duration::from_millis(5));
        app.next_animation_tick = Some(now + Duration::from_millis(100));
        app.session_save_deadline = Some(now + Duration::from_millis(200));
        assert_eq!(
            app.next_loop_deadline(now, false),
            app.selection_autoscroll_deadline
        );
    }

    #[test]
    fn tick_selection_autoscroll_self_heals_when_state_cleared() {
        let mut app = test_app();
        let now = Instant::now();
        app.state.selection_autoscroll = None;
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[test]
    fn tick_selection_autoscroll_stops_on_rect_change() {
        let mut app = test_app();
        let now = Instant::now();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.state.workspaces.push(ws);
        app.state.active = Some(0);
        app.state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        // Set autoscroll with a stale inner_rect that doesn't match pane_infos
        app.state.selection_autoscroll = Some(state::SelectionAutoscroll {
            direction: state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 999,
            inner_rect: ratatui::layout::Rect::new(0, 0, 1, 1), // wrong rect
        });
        app.selection_autoscroll_deadline = Some(now);
        app.tick_selection_autoscroll(now);
        assert!(app.state.selection_autoscroll.is_none());
        assert!(app.selection_autoscroll_deadline.is_none());
    }

    #[tokio::test]
    async fn full_internal_event_queue_eventually_applies_working_to_idle_transition() {
        let mut app = test_app();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;

        app.state.workspaces = vec![ws];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        assert_eq!(
            app.state.terminals.get(&terminal_id).unwrap().state,
            AgentState::Working
        );

        for i in 0..APP_EVENT_CHANNEL_CAPACITY {
            app.event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("9.9.{i}"),
                    install_command: "herdr update".into(),
                })
                .unwrap();
        }

        let tx = app.event_tx.clone();
        let send = tx.send(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
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

        let max_drains = (APP_EVENT_CHANNEL_CAPACITY / APP_EVENT_DRAIN_LIMIT) + 2;
        for _ in 0..max_drains {
            if app.state.terminals.get(&terminal_id).unwrap().state == AgentState::Idle {
                break;
            }
            app.drain_internal_events();
        }

        assert_eq!(
            app.state.terminals.get(&terminal_id).unwrap().state,
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
        app.state.detach_exits = false;

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
        app.state.detach_exits = false;

        // Start in terminal mode (default after workspace creation).
        app.state.mode = Mode::Terminal;
        assert!(!app.state.detach_requested);

        // Send Ctrl+B (prefix key, raw byte 0x02).
        let prefix_bytes = vec![0x02];
        app.route_client_input(prefix_bytes);

        assert_eq!(
            app.state.mode,
            Mode::Prefix,
            "prefix key should enter prefix mode"
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

    #[test]
    fn route_client_input_prefix_tab_dispatches_global_last_pane() {
        let config: Config = toml::from_str(
            r#"
[keys]
last_pane = "prefix+tab"
"#,
        )
        .unwrap();
        let mut app = test_app();
        let mut first = Workspace::test_new("one");
        let first_second_tab = first.test_add_tab(Some("logs"));
        let first_second_root = first.tabs[first_second_tab].root_pane;
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;
        app.state.workspaces = vec![first, second];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.keybinds = config.keybinds();
        app.state.mode = Mode::Terminal;
        app.state.switch_workspace_tab(0, first_second_tab);
        app.state.switch_workspace_tab(1, 0);

        app.route_client_input(vec![0x02, b'\t']);

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        app.route_client_input(vec![0x02, b'\t']);

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[tokio::test]
    async fn route_client_input_double_prefix_passes_prefix_through_to_focused_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prefix_code = KeyCode::Char('l');
        app.state.prefix_mods = KeyModifiers::CONTROL;

        app.route_client_input(vec![0x0c]);
        assert_eq!(app.state.mode, Mode::Prefix);

        app.route_client_input(vec![0x0c]);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from(vec![0x0c]));
    }

    #[tokio::test]
    async fn route_client_input_reencodes_terminal_keys_for_focused_pane_protocol() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        // Ghostty/kitty-style Ctrl-C should be normalized back to the pane's
        // negotiated encoding instead of being forwarded verbatim.
        app.route_client_input(b"\x1b[99;5u".to_vec());

        assert_eq!(rx.recv().await.unwrap(), bytes::Bytes::from(vec![3]));

        // iTerm2 and rxvt-style hosts may send F4 as CSI 14~. Normalize it
        // through the same semantic key path instead of leaking host bytes.
        app.route_client_input(b"\x1b[14~".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1bOS")
        );
    }

    #[tokio::test]
    async fn route_client_input_preserves_shift_enter_for_modify_other_keys_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_and_scrollback_bytes(80, 24, 0, b"\x1b[>4;1m", 4);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.route_client_input(b"\x1b[13;2u".to_vec());

        assert_eq!(
            rx.recv().await.unwrap(),
            bytes::Bytes::from_static(b"\x1b[27;2;13~")
        );
    }

    #[tokio::test]
    async fn route_client_input_splits_multi_event_payloads_before_forwarding() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel(80, 24);
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

    #[tokio::test]
    async fn route_client_input_forwards_multilingual_ime_text_to_focused_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let text = "中日한🙂";
        let (runtime, mut rx) =
            TerminalRuntime::test_with_channel_capacity(80, 24, text.chars().count());
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.route_client_input(text.as_bytes().to_vec());

        let mut forwarded = Vec::new();
        for _ in text.chars() {
            let chunk = rx.recv().await.unwrap();
            forwarded.extend_from_slice(&chunk);
        }
        assert_eq!(forwarded, text.as_bytes());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn route_client_input_forwards_long_voice_like_cjk_text_without_truncation() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let text = "你好，今天我们测试一段比较长的语音输入。こんにちは。안녕하세요.🙂".repeat(64);
        let char_count = text.chars().count();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel_capacity(80, 24, char_count);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.route_client_input(text.as_bytes().to_vec());

        let mut forwarded = Vec::new();
        for _ in 0..char_count {
            let chunk = rx.recv().await.unwrap();
            forwarded.extend_from_slice(&chunk);
        }
        assert_eq!(forwarded, text.as_bytes());
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
        assert_eq!(
            app.state.settings.section,
            state::SettingsSection::Integrations
        );
    }

    #[test]
    fn route_client_input_pastes_bracketed_text_into_rename_modal() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameTab;
        app.state.name_input = "2".into();
        app.state.name_input_replace_on_type = true;

        app.route_client_input(b"\x1b[200~feature/logs\x1b[201~".to_vec());

        assert_eq!(app.state.name_input, "feature/logs");
        assert!(!app.state.name_input_replace_on_type);
    }

    #[test]
    fn route_client_input_rename_enter_submits_through_api_path() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("old")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameWorkspace;
        app.state.name_input = "new".into();

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.workspaces[0].custom_name.as_deref(), Some("new"));
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceRenamed)
        }));
    }

    #[test]
    fn route_client_input_context_menu_enter_submits_through_api_path() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.confirm_close = false;
        app.state.context_menu = Some(state::ContextMenuState {
            kind: state::ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: state::MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;

        app.route_client_input(b"\r".to_vec());

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[test]
    fn raw_ctrl_v_decodes_as_modal_paste_shortcut() {
        let events = crate::raw_input::parse_raw_input_bytes_sync(&[0x16]);
        let Some(crate::raw_input::RawInputEvent::Key(key)) = events.first() else {
            panic!("expected ctrl-v key event");
        };

        assert!(input::is_modal_paste_shortcut(&key.as_key_event()));
    }

    #[test]
    fn route_client_events_pastes_text_into_new_linked_worktree_modal() {
        let mut app = test_app();
        app.state.mode = Mode::NewLinkedWorktree;
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(state::WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "generated-branch".into(),
            checkout_path: "/repo/herdr-generated-branch".into(),
            error: None,
            creating: false,
        });

        app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste(
                "feature/linear-302".into(),
            )],
            true,
        );

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
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

    #[tokio::test]
    async fn route_client_input_does_not_forward_incomplete_osc_introducer_to_pane() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) = TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.route_client_input(b"\x1b]".to_vec());

        assert!(rx.try_recv().is_err());
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
