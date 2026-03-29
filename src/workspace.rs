use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::layout::Direction;
use tokio::sync::mpsc;
use tracing::info;

use crate::detect::{self, Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::{PaneId, TileLayout};
use crate::pane::{PaneRuntime, PaneState};

/// A named workspace containing tiled terminal panes.
pub struct Workspace {
    /// User-provided override. If set, auto-derived identity stops updating.
    pub custom_name: Option<String>,
    /// Identity source for this workspace.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    /// Stable-ish public pane numbers within this workspace.
    /// New panes append at the end; closing a pane compacts higher numbers down.
    pub public_pane_numbers: HashMap<PaneId, usize>,
    pub(crate) next_public_pane_number: usize,
    /// Pane state — always present, testable without PTYs.
    pub panes: HashMap<PaneId, PaneState>,
    /// Pane runtimes — only present in production (empty in tests).
    pub runtimes: HashMap<PaneId, PaneRuntime>,
    pub zoomed: bool,
    pub events: mpsc::Sender<AppEvent>,
}

impl Workspace {
    pub fn new(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        events: mpsc::Sender<AppEvent>,
    ) -> std::io::Result<Self> {
        let (layout, root_id) = TileLayout::new();
        let runtime = PaneRuntime::spawn(root_id, rows, cols, initial_cwd, events.clone())?;

        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(root_id, 1);
        let mut runtimes = HashMap::new();
        runtimes.insert(root_id, runtime);

        info!(root_pane = root_id.raw(), "workspace created");
        Ok(Self {
            custom_name: None,
            root_pane: root_id,
            layout,
            public_pane_numbers,
            next_public_pane_number: 2,
            panes,
            runtimes,
            zoomed: false,
            events,
        })
    }

    /// Split the focused pane. Returns the new pane id.
    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
    ) -> std::io::Result<PaneId> {
        let new_id = self.layout.split_focused(direction);
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let runtime = PaneRuntime::spawn(new_id, rows, cols, actual_cwd, self.events.clone())?;
        self.panes.insert(new_id, PaneState::new());
        self.public_pane_numbers
            .insert(new_id, self.next_public_pane_number);
        self.next_public_pane_number += 1;
        self.runtimes.insert(new_id, runtime);
        self.zoomed = false;
        Ok(new_id)
    }

    /// Close the focused pane. Returns the removed pane id, or None if last pane.
    pub fn close_focused(&mut self) -> Option<PaneId> {
        let pane_id = self.layout.focused();
        self.remove_pane(pane_id)
    }

    /// Remove a specific pane from this workspace.
    /// Returns None if it's the last pane and the whole workspace should close.
    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<PaneId> {
        if self.layout.pane_count() <= 1 {
            return None;
        }

        let next_root = self.promoted_root_if_needed(pane_id);

        if self.layout.focused() == pane_id {
            self.layout.close_focused();
        } else {
            let prev_focus = self.layout.focused();
            self.layout.focus_pane(pane_id);
            self.layout.close_focused();
            self.layout.focus_pane(prev_focus);
        }

        if let Some(removed_number) = self.public_pane_numbers.remove(&pane_id) {
            for number in self.public_pane_numbers.values_mut() {
                if *number > removed_number {
                    *number -= 1;
                }
            }
            self.next_public_pane_number = self.public_pane_numbers.len() + 1;
        }
        self.panes.remove(&pane_id);
        self.runtimes.remove(&pane_id);
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some(pane_id)
    }

    fn promoted_root_if_needed(&self, closing: PaneId) -> Option<PaneId> {
        if self.root_pane != closing {
            return None;
        }
        self.layout.pane_ids().into_iter().find(|id| *id != closing)
    }

    pub fn public_pane_number(&self, pane_id: PaneId) -> Option<usize> {
        self.public_pane_numbers.get(&pane_id).copied()
    }

    /// Get the runtime for the focused pane.
    pub fn focused_runtime(&self) -> Option<&PaneRuntime> {
        self.runtimes.get(&self.layout.focused())
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn display_name(&self) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }

        self.root_cwd()
            .as_deref()
            .map(derive_label_from_cwd)
            .unwrap_or_else(|| "shell".to_string())
    }

    pub fn root_cwd(&self) -> Option<PathBuf> {
        self.runtimes.get(&self.root_pane).and_then(|rt| rt.cwd())
    }

    pub fn agent_summary(&self) -> Option<String> {
        let mut names = Vec::new();

        for id in self.layout.pane_ids() {
            if let Some(agent) = self.panes.get(&id).and_then(|p| p.detected_agent) {
                let name = agent_name(agent);
                if !names.iter().any(|n| *n == name) {
                    names.push(name);
                }
            }
        }

        if names.is_empty() {
            if self.root_cwd().is_some() {
                Some("shell".to_string())
            } else {
                None
            }
        } else if names.len() == 1 {
            Some(names[0].to_string())
        } else if names.len() == 2 {
            Some(format!("{}, {}", names[0], names[1]))
        } else {
            Some(format!("{}, {} +{}", names[0], names[1], names.len() - 2))
        }
    }

    /// Aggregate state + seen across all panes.
    /// Returns the highest-priority state and the worst-case seen flag.
    pub fn aggregate_state(&self) -> (AgentState, bool) {
        let states: Vec<AgentState> = self.panes.values().map(|p| p.state).collect();
        let state = detect::workspace_state(&states);
        let seen = self.panes.values().all(|p| p.seen);
        (state, seen)
    }

    /// Per-pane (state, seen) in BSP tree order (left-to-right, top-to-bottom).
    pub fn pane_states(&self) -> Vec<(AgentState, bool)> {
        self.layout
            .pane_ids()
            .iter()
            .map(|id| {
                self.panes
                    .get(id)
                    .map(|p| (p.state, p.seen))
                    .unwrap_or((AgentState::Unknown, true))
            })
            .collect()
    }
}

fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "pi",
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Gemini => "gemini",
        Agent::Cursor => "cursor",
        Agent::Cline => "cline",
        Agent::OpenCode => "opencode",
        Agent::GithubCopilot => "copilot",
        Agent::Kimi => "kimi",
        Agent::Droid => "droid",
        Agent::Amp => "amp",
    }
}

fn derive_label_from_cwd(cwd: &Path) -> String {
    if let Some(repo_root) = git_repo_root(cwd) {
        if let Some(name) = repo_root.file_name().and_then(|n| n.to_str()) {
            return name.to_string();
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let home = Path::new(&home);
        if cwd == home {
            return "~".to_string();
        }
    }

    cwd.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.display().to_string())
}

fn git_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers — construct workspaces without PTYs
// ---------------------------------------------------------------------------

#[cfg(test)]
impl Workspace {
    /// Create a test workspace with one pane, no PTY runtime.
    pub fn test_new(name: &str) -> Self {
        let (events, _) = mpsc::channel(64);
        let (layout, root_id) = TileLayout::new();
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(root_id, 1);
        Self {
            custom_name: Some(name.to_string()),
            root_pane: root_id,
            layout,
            public_pane_numbers,
            next_public_pane_number: 2,
            panes,
            runtimes: HashMap::new(),
            zoomed: false,
            events,
        }
    }

    /// Add a test pane (splits focused, no PTY runtime).
    pub fn test_split(&mut self, direction: Direction) -> PaneId {
        let new_id = self.layout.split_focused(direction);
        self.panes.insert(new_id, PaneState::new());
        self.public_pane_numbers
            .insert(new_id, self.next_public_pane_number);
        self.next_public_pane_number += 1;
        new_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentState;

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);

        let root_id = *ws.panes.keys().find(|id| **id != id2).unwrap();
        ws.panes.get_mut(&root_id).unwrap().state = AgentState::Idle;
        ws.panes.get_mut(&id2).unwrap().state = AgentState::Busy;

        let (state, _) = ws.aggregate_state();
        assert_eq!(state, AgentState::Busy);
    }

    #[test]
    fn aggregate_seen_any_unseen_means_unseen() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);

        ws.panes.get_mut(&id2).unwrap().seen = false;

        let (_, seen) = ws.aggregate_state();
        assert!(!seen);
    }

    #[test]
    fn close_focused_removes_pane() {
        let mut ws = Workspace::test_new("test");
        let _id2 = ws.test_split(Direction::Horizontal);
        assert_eq!(ws.panes.len(), 2);

        let closed = ws.close_focused();
        assert!(closed.is_some());
        assert_eq!(ws.panes.len(), 1);
    }

    #[test]
    fn close_focused_last_pane_returns_none() {
        let mut ws = Workspace::test_new("test");
        assert_eq!(ws.panes.len(), 1);

        let closed = ws.close_focused();
        assert!(closed.is_none());
        assert_eq!(ws.panes.len(), 1);
    }

    #[test]
    fn pane_states_matches_layout_order() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);

        ws.panes.get_mut(&id2).unwrap().state = AgentState::Waiting;

        let states = ws.pane_states();
        assert_eq!(states.len(), 2);
        assert!(states.iter().any(|(s, _)| *s == AgentState::Waiting));
        assert!(states.iter().any(|(s, _)| *s == AgentState::Unknown));
    }

    #[test]
    fn closing_root_promotes_another_pane() {
        let mut ws = Workspace::test_new("test");
        let root = ws.root_pane;
        let other = ws.test_split(Direction::Horizontal);
        ws.layout.focus_pane(root);
        ws.remove_pane(root);
        assert_eq!(ws.root_pane, other);
    }
}
