mod actions;
mod command;
mod config_edit;
mod env;
mod file_ops;
mod registry;
mod targets;
mod types;
mod version;

pub(crate) use actions::{install_target, uninstall_target};
#[cfg(test)]
pub(crate) use env::integration_env_lock;
pub(crate) use env::{
    apply_pane_base_env, HERDR_PANE_ID_ENV_VAR, HERDR_TAB_ID_ENV_VAR, HERDR_WORKSPACE_ID_ENV_VAR,
};
pub(crate) use registry::{
    installed_integration_statuses, integration_recommendations, integration_target_label,
    print_outdated_update_notice,
};
pub(crate) use types::{IntegrationRecommendation, IntegrationStatus, IntegrationStatusKind};

const PI_EXTENSION_INSTALL_NAME: &str = "herdr-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/herdr-agent-state.ts");
const PI_INTEGRATION_VERSION: u32 = 4;
const OMP_EXTENSION_INSTALL_NAME: &str = "herdr-omp-agent-state.ts";
const OMP_EXTENSION_ASSET: &str = include_str!("assets/omp/herdr-agent-state.ts");
const OMP_INTEGRATION_VERSION: u32 = 4;
const CLAUDE_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const CLAUDE_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/claude/herdr-agent-state.ps1")
} else {
    include_str!("assets/claude/herdr-agent-state.sh")
};
const CLAUDE_INTEGRATION_VERSION: u32 = 7;
const CODEX_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const CODEX_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/codex/herdr-agent-state.ps1")
} else {
    include_str!("assets/codex/herdr-agent-state.sh")
};
const CODEX_INTEGRATION_VERSION: u32 = 6;
const KIMI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const KIMI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/kimi/herdr-agent-state.ps1")
} else {
    include_str!("assets/kimi/herdr-agent-state.sh")
};
const KIMI_INTEGRATION_VERSION: u32 = 4;
const KIMI_CONFIG_BLOCK_BEGIN: &str = "# >>> herdr kimi integration";
const KIMI_CONFIG_BLOCK_END: &str = "# <<< herdr kimi integration";
const KIMI_MIN_VERSION: &str = "0.14.0";
const KIMI_HOOK_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("SubagentStart", "working"),
    ("PreCompact", "working"),
    ("PermissionRequest", "blocked"),
    ("PermissionResult", "working"),
    ("Stop", "idle"),
    ("Interrupt", "idle"),
];
const COPILOT_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const COPILOT_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/copilot/herdr-agent-state.ps1")
} else {
    include_str!("assets/copilot/herdr-agent-state.sh")
};
const COPILOT_INTEGRATION_VERSION: u32 = 2;
const COPILOT_HOOK_EVENTS: [&str; 1] = ["SessionStart"];
const COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS: [&str; 9] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "agentStop",
    "SessionEnd",
    "notification",
    "sessionStart",
];
const DEVIN_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const DEVIN_HOOK_ASSET: &str = include_str!("assets/devin/herdr-agent-state.sh");
const DEVIN_INTEGRATION_VERSION: u32 = 2;
const DEVIN_HOOK_EVENTS: [(&str, &str); 6] = [
    ("SessionStart", "session"),
    ("UserPromptSubmit", "session"),
    ("PreToolUse", "session"),
    ("PostToolUse", "session"),
    ("PermissionRequest", "session"),
    ("Stop", "session"),
];
const DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 6] = [
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const DROID_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const DROID_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/droid/herdr-agent-state.ps1")
} else {
    include_str!("assets/droid/herdr-agent-state.sh")
};
const DROID_INTEGRATION_VERSION: u32 = 2;
const DROID_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const DROID_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("Notification", "blocked"),
    ("Stop", "idle"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("SessionEnd", "release"),
];
const OPENCODE_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/herdr-agent-state.js");
const OPENCODE_INTEGRATION_VERSION: u32 = 8;
const KILO_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
const KILO_PLUGIN_ASSET: &str = include_str!("assets/kilo/herdr-agent-state.js");
const KILO_INTEGRATION_VERSION: u32 = 2;
const HERMES_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state";
const HERMES_PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
const HERMES_PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
const HERMES_PLUGIN_MANIFEST_ASSET: &str = include_str!("assets/hermes/plugin.yaml");
const HERMES_PLUGIN_INIT_ASSET: &str = include_str!("assets/hermes/__init__.py");
const HERMES_INTEGRATION_VERSION: u32 = 3;
const QODERCLI_HOOK_INSTALL_NAME: &str = if cfg!(windows) {
    "herdr-agent-state.ps1"
} else {
    "herdr-agent-state.sh"
};
const QODERCLI_HOOK_ASSET: &str = if cfg!(windows) {
    include_str!("assets/qodercli/herdr-agent-state.ps1")
} else {
    include_str!("assets/qodercli/herdr-agent-state.sh")
};
const QODERCLI_INTEGRATION_VERSION: u32 = 2;
const QODERCLI_HOOK_EVENTS: [(&str, &str); 1] = [("SessionStart", "session")];
const QODERCLI_REMOVED_LIFECYCLE_HOOK_EVENTS: [(&str, &str); 12] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PostToolUse", "working"),
    ("PostToolUseFailure", "working"),
    ("SubagentStart", "working"),
    ("SubagentStop", "working"),
    ("PreCompact", "working"),
    ("Notification", "blocked"),
    ("PermissionRequest", "blocked"),
    ("Stop", "idle"),
    ("SessionEnd", "release"),
];
const CURSOR_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const CURSOR_HOOK_ASSET: &str = include_str!("assets/cursor/herdr-agent-state.sh");
const CURSOR_INTEGRATION_VERSION: u32 = 1;
const INTEGRATION_VERSION_MARKER: &str = "HERDR_INTEGRATION_VERSION=";

pub(crate) const INSTALL_WARNING_PREFIX: &str = "warning:";

#[cfg(test)]
mod tests;
