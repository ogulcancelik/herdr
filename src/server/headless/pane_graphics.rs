use tracing::warn;

use super::{HeadlessServer, RenderImpact};
use crate::api;
use crate::protocol::{ServerMessage, MAX_GRAPHICS_FRAME_SIZE};
use crate::server::clients::{render_targets, ClientConnectionMode, DeferredRender};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetainedGraphicsOutcome {
    Sent,
    Deferred,
    Fallback,
}

pub(super) fn frame_pane_graphics_for_client(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.is_empty() {
        return bytes;
    }
    let mut framed = Vec::with_capacity(bytes.len() + 4);
    framed.extend_from_slice(b"\x1b7");
    framed.extend_from_slice(&bytes);
    framed.extend_from_slice(b"\x1b8");
    framed
}

impl HeadlessServer {
    pub(super) fn pane_graphics_runtime_active(&self) -> bool {
        !self.app.state.pane_graphics_layers.is_empty()
            || !self.app.state.pane_graphics_streams.is_empty()
    }

    pub(super) fn cancel_inactive_pane_graphics_streams(&self) {
        api::cancel_inactive_pane_graphics_streams(|owner| {
            self.app
                .state
                .pane_graphics_streams
                .values()
                .any(|active_owner| active_owner == owner)
        });
    }

    pub(super) fn handle_pane_graphics_stream_frame(
        &mut self,
        msg: api::ApiRequestMessage,
    ) -> RenderImpact {
        if self.shutting_down {
            let _ = self.handle_api_request_with_shutdown_check_inner(msg, false);
            return RenderImpact::None;
        }

        let internal_changed = self.drain_all_internal_events_with_forwarding();
        let response = self
            .app
            .handle_api_request_after_internal_events_drained(msg.request);
        let succeeded = serde_json::from_str::<api::schema::SuccessResponse>(&response).is_ok();
        let _ = msg.respond_to.send(response);

        if internal_changed {
            RenderImpact::Full
        } else if succeeded {
            RenderImpact::Graphics
        } else {
            RenderImpact::None
        }
    }

    pub(super) fn render_retained_graphics_update_and_stream(&mut self) -> RetainedGraphicsOutcome {
        crate::render_prof::event("retained_graphics.attempt");
        if self.app.full_redraw_pending {
            crate::render_prof::event("retained_graphics_fallback.full_redraw_pending");
            return RetainedGraphicsOutcome::Fallback;
        }

        let render_targets = render_targets(&self.clients, self.foreground_client_id);
        let mut app_view_size = None;
        for (_, terminal_size, _, _, mode) in &render_targets {
            if !matches!(mode, ClientConnectionMode::App) {
                continue;
            }
            if app_view_size.is_some_and(|size| size != *terminal_size) {
                crate::render_prof::event("retained_graphics_fallback.mixed_app_geometry");
                return RetainedGraphicsOutcome::Fallback;
            }
            app_view_size = Some(*terminal_size);
        }
        let mut deferred = false;
        let mut prepared = Vec::new();

        for (client_id, (cols, rows), cell_size, _is_foreground, mode) in render_targets {
            if !matches!(mode, ClientConnectionMode::App) {
                continue;
            }
            let Some(client) = self.clients.get_mut(&client_id) else {
                crate::render_prof::event("retained_graphics_fallback.client_missing");
                return RetainedGraphicsOutcome::Fallback;
            };
            if client.deferred_render() != DeferredRender::None {
                deferred = true;
                continue;
            }
            if client.graphics_surface_reset_pending || !cell_size.is_known() {
                crate::render_prof::event("retained_graphics_fallback.client_state");
                return RetainedGraphicsOutcome::Fallback;
            }
            let Some(last_frame) = client.render_state.last_frame() else {
                crate::render_prof::event("retained_graphics_fallback.no_last_frame");
                return RetainedGraphicsOutcome::Fallback;
            };
            if last_frame.width != cols || last_frame.height != rows {
                crate::render_prof::event("retained_graphics_fallback.frame_size_mismatch");
                return RetainedGraphicsOutcome::Fallback;
            }
            if client.writer.is_none() {
                crate::render_prof::event("retained_graphics_fallback.writer_missing");
                return RetainedGraphicsOutcome::Fallback;
            }

            let mut next_graphics_cache = client.graphics_cache.clone();
            let encode_started = crate::render_prof::timer();
            let bytes =
                frame_pane_graphics_for_client(crate::kitty_graphics::encode_local_pane_graphics(
                    &self.app.state,
                    &self.app.terminal_runtimes,
                    self.app.state.view.tab_surface(),
                    cell_size,
                    &mut next_graphics_cache,
                ));
            crate::render_prof::duration_since("retained_graphics.graphics_encode", encode_started);
            if bytes.len() > MAX_GRAPHICS_FRAME_SIZE {
                warn!(
                    client_id,
                    graphics_bytes = bytes.len(),
                    max = MAX_GRAPHICS_FRAME_SIZE,
                    "dropping oversized retained graphics payload"
                );
                continue;
            }
            let serialized = if bytes.is_empty() {
                None
            } else {
                let serialize_started = crate::render_prof::timer();
                let framed = match Self::frame_server_message_with_max(
                    &ServerMessage::Graphics { bytes },
                    MAX_GRAPHICS_FRAME_SIZE,
                ) {
                    Ok(framed) => framed,
                    Err(err) => {
                        warn!(client_id, err = %err, "failed to serialize retained graphics");
                        return RetainedGraphicsOutcome::Fallback;
                    }
                };
                crate::render_prof::duration_since(
                    "retained_graphics.serialize",
                    serialize_started,
                );
                Some(framed)
            };
            prepared.push((client_id, serialized, next_graphics_cache));
        }

        let mut broken_clients = Vec::new();
        for (client_id, serialized, next_graphics_cache) in prepared {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(serialized) = serialized else {
                client.graphics_cache = next_graphics_cache;
                client.clear_deferred_render();
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                broken_clients.push(client_id);
                continue;
            };
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    client.graphics_cache = next_graphics_cache;
                    client.clear_deferred_render();
                    crate::render_prof::event("retained_graphics.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_pane_graphics_render();
                    deferred = true;
                    crate::render_prof::event("retained_graphics.deferred");
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }
        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }

        if deferred {
            RetainedGraphicsOutcome::Deferred
        } else {
            RetainedGraphicsOutcome::Sent
        }
    }
}
