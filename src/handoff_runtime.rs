#[cfg(unix)]
use serde::{Deserialize, Serialize};

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
    /// Monotonic output revision carried across handoff so subscribers'
    /// `PaneOutputChanged` cursors stay valid after respawn (Phase 5).
    #[serde(default)]
    pub output_revision: u64,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: std::os::fd::RawFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}

#[cfg(all(test, unix))]
mod tests {
    use super::HandoffRuntimeState;

    #[test]
    fn handoff_state_round_trips_output_revision() {
        let state = HandoffRuntimeState {
            pane_id: 7,
            child_pid: 1234,
            rows: 24,
            cols: 80,
            cell_width_px: 8,
            cell_height_px: 16,
            keyboard_protocol_flags: 0,
            keyboard_protocol_ansi: None,
            input_state: None,
            initial_history_ansi: None,
            output_revision: 42,
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: HandoffRuntimeState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.output_revision, 42);
    }

    #[test]
    fn handoff_state_old_payload_defaults_output_revision_to_zero() {
        // Payload shape emitted by servers that predate the Phase 5 field.
        let json = r#"{
            "pane_id": 7,
            "child_pid": 1234,
            "rows": 24,
            "cols": 80,
            "cell_width_px": 8,
            "cell_height_px": 16
        }"#;
        let decoded: HandoffRuntimeState = serde_json::from_str(json).unwrap();
        assert_eq!(decoded.output_revision, 0);
    }
}
