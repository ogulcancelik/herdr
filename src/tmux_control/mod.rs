//! Tmux control mode integration.
//!
//! Provides push-based event monitoring for tmux sessions via `tmux -C`.
//! This supplements herdr's existing screen-heuristic and hook-based
//! agent detection by receiving real-time pane output and lifecycle events
//! directly from the tmux server.
//!
//! # Architecture
//!
//! ```text
//! tmux -C (control mode)
//!   ├── %output events → protocol parser → AppEvent::TmuxPaneOutput
//!   ├── %window-add/close → lifecycle tracking
//!   └── %pane-focus-changed → focus tracking
//! ```
//!
//! # Configuration
//!
//! Add to `config.toml`:
//!
//! ```toml
//! [tmux_control]
//! enabled = true
//! target_sessions = []        # empty = all sessions
//! socket_path = null          # null = default tmux socket
//! ```

pub mod listener;
pub mod protocol;

pub use listener::{TmuxControlConfig, TmuxControlListener};
pub use protocol::TmuxEvent;

/// Convert a tmux control mode event into a herdr app event.
///
/// This maps `TmuxEvent::Output` events to the existing detection pipeline
/// by providing the raw pane output for pattern matching.
pub fn event_to_detection_input(event: &TmuxEvent) -> Option<TmuxDetectionInput> {
    match event {
        TmuxEvent::Output { pane, data } => {
            // Strip ANSI codes for text-based detection.
            let plain = protocol::strip_ansi(data);
            Some(TmuxDetectionInput {
                pane_id: pane.clone(),
                raw_data: data.clone(),
                plain_text: plain,
            })
        }
        _ => None,
    }
}

/// Input data for agent detection from tmux control mode output.
#[derive(Debug, Clone)]
pub struct TmuxDetectionInput {
    /// The tmux pane ID (e.g., "42" from "%42").
    pub pane_id: String,
    /// Raw output data with ANSI escape sequences.
    pub raw_data: String,
    /// Plain text with ANSI codes stripped.
    pub plain_text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_event_maps_to_detection_input() {
        let event = TmuxEvent::Output {
            pane: "42".into(),
            data: "Thinking…\n".into(),
        };
        let input = event_to_detection_input(&event).unwrap();
        assert_eq!(input.pane_id, "42");
        assert_eq!(input.plain_text, "Thinking…\n");
    }

    #[test]
    fn non_output_events_return_none() {
        let event = TmuxEvent::SessionCreated { session: "test".into() };
        assert!(event_to_detection_input(&event).is_none());
    }
}
