use std::collections::HashMap;
use std::path::PathBuf;

use crate::protocol::RenderEncoding;
use crate::server::client_transport::ClientWriter;
use crate::server::render_stream::ClientRenderState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientConnectionMode {
    App,
    TerminalAttach { terminal_id: String },
    TerminalObserve { terminal_id: String },
}

pub(crate) type RenderTarget = (
    u64,
    (u16, u16),
    crate::kitty_graphics::HostCellSize,
    bool,
    ClientConnectionMode,
);

/// A connected client tracked by the server.
pub(crate) struct ClientConnection {
    /// Whether this connection is the full app client or a direct terminal attach.
    pub(crate) mode: ClientConnectionMode,
    /// True after the handshake for clients that will switch into direct terminal attach mode.
    pub(crate) pending_terminal_attach: bool,
    /// Client-local app keybindings. None means use the server's keybindings.
    pub(crate) keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
    /// The client's terminal size after clamping.
    pub(crate) terminal_size: (u16, u16),
    /// Pixel size of one client terminal cell.
    pub(crate) cell_size: crate::kitty_graphics::HostCellSize,
    /// Last known host terminal default colors for this client.
    pub(crate) host_terminal_theme: crate::terminal_theme::TerminalTheme,
    /// Last known host terminal appearance for this client.
    pub(crate) host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    /// True when appearance came from an explicit host color-scheme report.
    pub(crate) host_terminal_appearance_explicit: bool,
    /// Last reported focus state for this client's outer terminal.
    pub(crate) outer_terminal_focus: Option<bool>,
    /// Stateful parser for app-client input split across transport reads.
    pub(crate) raw_input: crate::raw_input::RawInputFramer,
    /// Monotonic activity stamp used to choose the fallback foreground client.
    pub(crate) last_activity: u64,
    /// Render baseline for the negotiated client encoding.
    pub(crate) render_state: ClientRenderState,
    /// Client-local host Kitty graphics cache.
    pub(crate) graphics_cache: crate::kitty_graphics::HostGraphicsCache,
    /// Whether the next graphics frame must clear and rebuild host-side Kitty state.
    pub(crate) graphics_surface_reset_pending: bool,
    /// Whether a render was skipped because the render channel was full.
    pub(crate) render_pending: bool,
    /// Last host mouse capture mode sent to this client.
    pub(crate) host_mouse_capture_active: Option<bool>,
    /// Temporary files staged from this client's local clipboard image pastes.
    pub(crate) staged_clipboard_files: Vec<PathBuf>,
    /// Channels for sending framed ServerMessage data to the client writer thread.
    pub(crate) writer: Option<ClientWriter>,
}

impl ClientConnection {
    #[cfg(test)]
    pub(crate) fn new(
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self::new_with_mode(
            ClientConnectionMode::App,
            None,
            terminal_size,
            cell_size,
            host_terminal_theme,
            outer_terminal_focus,
            last_activity,
            render_encoding,
            false,
            writer,
        )
    }

    pub(crate) fn new_with_mode(
        mode: ClientConnectionMode,
        keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        pending_terminal_attach: bool,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self {
            mode,
            pending_terminal_attach,
            keybindings,
            terminal_size,
            cell_size,
            host_terminal_appearance: host_terminal_theme
                .background
                .map(crate::terminal_theme::RgbColor::inferred_appearance),
            host_terminal_appearance_explicit: false,
            host_terminal_theme,
            outer_terminal_focus,
            raw_input: crate::raw_input::RawInputFramer::default(),
            last_activity,
            render_state: ClientRenderState::new(render_encoding),
            graphics_cache: crate::kitty_graphics::HostGraphicsCache::default(),
            graphics_surface_reset_pending: false,
            render_pending: false,
            host_mouse_capture_active: None,
            staged_clipboard_files: Vec::new(),
            writer,
        }
    }

    pub(crate) fn request_full_redraw(&mut self) {
        self.render_state.reset_baseline();
        self.graphics_surface_reset_pending = true;
    }

    pub(crate) fn is_full_app_client(&self) -> bool {
        matches!(self.mode, ClientConnectionMode::App) && !self.pending_terminal_attach
    }

    pub(crate) fn request_semantic_redraw_after_input(&mut self) {
        self.render_state.reset_semantic_input_baseline();
    }

    pub(crate) fn update_host_theme_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let mut next_theme = self.host_terminal_theme;
        let mut changed = false;
        for event in events {
            match event {
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    next_theme = next_theme.with_color(*kind, *color);
                    if matches!(kind, crate::terminal_theme::DefaultColorKind::Background)
                        && !self.host_terminal_appearance_explicit
                    {
                        changed |=
                            self.set_host_appearance(Some(color.inferred_appearance()), false);
                    }
                }
                crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                    changed |= self.set_host_appearance(Some(*appearance), true);
                }
                _ => {}
            }
        }

        if next_theme != self.host_terminal_theme {
            self.host_terminal_theme = next_theme;
            changed = true;
        }
        changed
    }

    fn set_host_appearance(
        &mut self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
        explicit: bool,
    ) -> bool {
        if self.host_terminal_appearance_explicit && !explicit {
            return false;
        }
        if self.host_terminal_appearance == appearance
            && self.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        self.host_terminal_appearance = appearance;
        self.host_terminal_appearance_explicit = explicit;
        true
    }

    pub(crate) fn update_outer_focus_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> Option<bool> {
        let next_focus = events
            .iter()
            .filter_map(|event| match event {
                crate::raw_input::RawInputEvent::OuterFocusGained => Some(true),
                crate::raw_input::RawInputEvent::OuterFocusLost => Some(false),
                _ => None,
            })
            .next_back()?;

        self.outer_terminal_focus = Some(next_focus);
        Some(next_focus)
    }
}

pub(crate) fn events_include_interaction(events: &[crate::raw_input::RawInputEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            crate::raw_input::RawInputEvent::Key(_)
                | crate::raw_input::RawInputEvent::Mouse(_)
                | crate::raw_input::RawInputEvent::Paste(_)
                | crate::raw_input::RawInputEvent::OuterFocusGained
        )
    })
}

pub(crate) fn latest_app_client(clients: &HashMap<u64, ClientConnection>) -> Option<u64> {
    clients
        .iter()
        .filter(|(_, client)| client.is_full_app_client())
        .max_by_key(|(_, client)| client.last_activity)
        .map(|(&client_id, _)| client_id)
}

pub(crate) fn terminal_stream_client_ids(
    clients: &HashMap<u64, ClientConnection>,
    terminal_id: &str,
) -> Vec<u64> {
    clients
        .iter()
        .filter_map(|(&client_id, client)| match &client.mode {
            ClientConnectionMode::TerminalAttach {
                terminal_id: attached,
            }
            | ClientConnectionMode::TerminalObserve {
                terminal_id: attached,
            } if attached == terminal_id => Some(client_id),
            _ => None,
        })
        .collect()
}

pub(crate) fn render_targets(
    clients: &HashMap<u64, ClientConnection>,
    foreground_client_id: Option<u64>,
) -> Vec<RenderTarget> {
    let mut targets: Vec<RenderTarget> = clients
        .iter()
        .filter(|(_, client)| {
            client.writer.is_some()
                && (client.is_full_app_client()
                    || matches!(
                        client.mode,
                        ClientConnectionMode::TerminalAttach { .. }
                            | ClientConnectionMode::TerminalObserve { .. }
                    ))
        })
        .map(|(&client_id, client)| {
            (
                client_id,
                client.terminal_size,
                client.cell_size,
                foreground_client_id == Some(client_id),
                client.mode.clone(),
            )
        })
        .collect();

    targets.sort_by_key(|(client_id, _, _, is_foreground, _)| (*is_foreground, *client_id));
    targets
}
