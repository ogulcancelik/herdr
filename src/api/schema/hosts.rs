use serde::{Deserialize, Serialize};

/// `host` is an ssh alias/target identifying the remote herdr server to
/// attach. No `--handoff`-style options here (YAGNI per the multi-host plan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostAttachParams {
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDetachParams {
    pub host: String,
}

/// One attached host's link state and adopted pane count, as reported by
/// `host.list`.
///
/// `state` is a snake_case label (`"connecting"`, `"connected"`,
/// `"reconnecting"`, `"offline"`) converted from the server's internal
/// `server::host_link::LinkState` at the API boundary -- that internal
/// newtype (and `HostLinkId`) never crosses into this pub schema, matching
/// the boundary rule the Task 6 schema review established for other server
/// newtypes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostInfo {
    pub host: String,
    pub state: String,
    pub pane_count: u32,
}
