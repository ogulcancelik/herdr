//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct WorktreeKillGateResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub branch: Option<String>,
    pub gate: crate::worktree::WorktreeMergeGate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeBranchDeleteResult {
    pub branch: String,
    pub result: Result<(), String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub result: Result<(), String>,
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A pane's child process exited.
    PaneDied {
        pane_id: PaneId,
    },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        /// Free-text activity from the agent's status line while Working.
        activity: Option<String>,
        visible_blocker: bool,
        visible_idle: bool,
        visible_working: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// Periodic machine metrics snapshot for the global status line.
    SystemStatsUpdated(crate::system_stats::SystemStats),
    /// The slow PR-state poll interval elapsed; collect targets and query gh.
    PrStatePollDue,
    /// Background gh results: per-workspace PR state for worktree branches.
    PrStatesUpdated(Vec<(String, Option<crate::worktree::PrStateInfo>)>),
    /// A user prompt submitted to an agent pane (integration hook report).
    HookPromptReported {
        pane_id: PaneId,
        prompt: String,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Display-only agent metadata was reported for a pane.
    HookMetadataReported {
        pane_id: PaneId,
        source: String,
        agent_label: Option<String>,
        applies_to_source: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
        custom_status: Option<String>,
        state_labels: std::collections::HashMap<String, String>,
        clear_title: bool,
        clear_display_agent: bool,
        clear_custom_status: bool,
        clear_state_labels: bool,
        seq: Option<u64>,
        ttl: Option<std::time::Duration>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite {
        content: Vec<u8>,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
    },
    /// Background `git worktree add` completed.
    WorktreeAddFinished(WorktreeAddResult),
    /// Background `git worktree remove` completed.
    WorktreeRemoveFinished(WorktreeRemoveResult),
    WorktreeKillGateFinished(WorktreeKillGateResult),
    WorktreeBranchDeleteFinished(WorktreeBranchDeleteResult),
}
