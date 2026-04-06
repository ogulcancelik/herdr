use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::info;

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::{PaneId, TileLayout};
use crate::pane::{PaneRuntime, PaneState};

static NEXT_WORKSPACE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn generate_workspace_id() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or(0);
    let counter = NEXT_WORKSPACE_ID.fetch_add(1, Ordering::Relaxed);
    format!("w{micros:x}{counter:x}")
}

pub struct Tab {
    pub custom_name: Option<String>,
    pub number: usize,
    /// Identity source for this tab's pane tree.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    /// Pane state — always present, testable without PTYs.
    pub panes: HashMap<PaneId, PaneState>,
    /// Best-effort cwd cache per pane. Live runtime cwd wins when available.
    pub pane_cwds: HashMap<PaneId, PathBuf>,
    /// Pane runtimes — only present in production (empty in tests).
    pub runtimes: HashMap<PaneId, PaneRuntime>,
    pub zoomed: bool,
    pub events: mpsc::Sender<AppEvent>,
    pub(crate) render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<AtomicBool>,
}

impl Drop for Tab {
    fn drop(&mut self) {
        let runtimes = std::mem::take(&mut self.runtimes);
        for (pane_id, runtime) in runtimes {
            runtime.shutdown(pane_id);
        }
    }
}

impl Tab {
    pub fn new(
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let (layout, root_id) = TileLayout::new();
        let runtime = PaneRuntime::spawn(
            root_id,
            rows,
            cols,
            initial_cwd.clone(),
            scrollback_limit_bytes,
            host_terminal_theme,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        )?;

        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let mut pane_cwds = HashMap::new();
        pane_cwds.insert(root_id, initial_cwd);
        let mut runtimes = HashMap::new();
        runtimes.insert(root_id, runtime);

        Ok(Self {
            custom_name: None,
            number,
            root_pane: root_id,
            layout,
            panes,
            pane_cwds,
            runtimes,
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        })
    }

    pub fn display_name(&self) -> String {
        self.custom_name
            .clone()
            .unwrap_or_else(|| self.number.to_string())
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
    ) -> std::io::Result<PaneId> {
        let new_id = self.layout.split_focused(direction);
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let runtime = PaneRuntime::spawn(
            new_id,
            rows,
            cols,
            actual_cwd.clone(),
            scrollback_limit_bytes,
            host_terminal_theme,
            self.events.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        self.panes.insert(new_id, PaneState::new());
        self.pane_cwds.insert(new_id, actual_cwd);
        self.runtimes.insert(new_id, runtime);
        self.zoomed = false;
        Ok(new_id)
    }

    pub fn close_focused(&mut self) -> Option<(PaneId, Option<PaneRuntime>)> {
        let pane_id = self.layout.focused();
        self.detach_pane(pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
        self.detach_pane(pane_id)
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
        self.detach_pane(pane_id)
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<(PaneId, Option<PaneRuntime>)> {
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

        self.panes.remove(&pane_id);
        self.pane_cwds.remove(&pane_id);
        let runtime = self.runtimes.remove(&pane_id);
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some((pane_id, runtime))
    }

    fn promoted_root_if_needed(&self, closing: PaneId) -> Option<PaneId> {
        if self.root_pane != closing {
            return None;
        }
        self.layout.pane_ids().into_iter().find(|id| *id != closing)
    }

    pub fn focused_runtime(&self) -> Option<&PaneRuntime> {
        self.runtimes.get(&self.layout.focused())
    }

    pub fn cwd_for_pane(&self, pane_id: PaneId) -> Option<PathBuf> {
        self.runtimes
            .get(&pane_id)
            .and_then(|rt| rt.cwd())
            .or_else(|| self.pane_cwds.get(&pane_id).cloned())
    }

    pub fn root_cwd(&self) -> Option<PathBuf> {
        self.cwd_for_pane(self.root_pane)
    }

    pub fn has_working_pane(&self) -> bool {
        self.panes
            .values()
            .any(|pane| pane.state == AgentState::Working)
    }

    #[cfg(test)]
    #[allow(dead_code)] // retained for focused layout-order assertions in tests
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

    pub fn pane_details(&self) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .filter_map(|id| {
                let pane = self.panes.get(id);
                let agent = pane.and_then(|p| p.detected_agent)?;
                let state = pane.map(|p| p.state).unwrap_or(AgentState::Unknown);
                let seen = pane.map(|p| p.seen).unwrap_or(true);
                let agent_label = agent_name(agent).to_string();
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx: self.number.saturating_sub(1),
                    tab_label: self.display_name(),
                    label: agent_label.clone(),
                    agent_label,
                    agent: Some(agent),
                    state,
                    seen,
                })
            })
            .collect()
    }
}

/// A named workspace containing tabs.
pub struct Workspace {
    /// Stable public workspace identity, independent of display order.
    pub id: String,
    /// User-provided override. If set, auto-derived identity stops updating.
    pub custom_name: Option<String>,
    /// Fallback workspace identity source for tests, old snapshots, or missing runtimes.
    pub identity_cwd: PathBuf,
    /// Cached ahead/behind counts for the workspace repo's current branch upstream.
    pub(crate) cached_git_ahead_behind: Option<(usize, usize)>,
    /// Stable-ish public pane numbers within this workspace.
    /// New panes append at the end; closing a pane compacts higher numbers down.
    pub public_pane_numbers: HashMap<PaneId, usize>,
    pub(crate) next_public_pane_number: usize,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
}

impl Deref for Workspace {
    type Target = Tab;

    fn deref(&self) -> &Self::Target {
        self.active_tab()
            .expect("workspace must always have at least one active tab")
    }
}

impl DerefMut for Workspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_tab_mut()
            .expect("workspace must always have at least one active tab")
    }
}

impl Workspace {
    pub fn new(
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let tab = Tab::new(
            1,
            initial_cwd.clone(),
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
        )?;
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(tab.root_pane, 1);
        info!(root_pane = tab.root_pane.raw(), "workspace created");
        Ok(Self {
            id: generate_workspace_id(),
            custom_name: None,
            identity_cwd: initial_cwd,
            cached_git_ahead_behind: None,
            public_pane_numbers,
            next_public_pane_number: 2,
            tabs: vec![tab],
            active_tab: 0,
        })
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active_tab)
    }

    pub fn active_tab_display_name(&self) -> Option<String> {
        self.active_tab().map(Tab::display_name)
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            if let Some(tab) = self.tabs.get_mut(idx) {
                for pane in tab.panes.values_mut() {
                    pane.seen = true;
                }
            }
        }
    }

    pub fn create_tab(
        &mut self,
        rows: u16,
        cols: u16,
        cwd: PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
    ) -> std::io::Result<usize> {
        let number = self.tabs.len() + 1;
        let events = self
            .active_tab()
            .map(|tab| tab.events.clone())
            .expect("workspace must always have at least one tab");
        let render_notify = self
            .active_tab()
            .map(|tab| tab.render_notify.clone())
            .expect("workspace must always have at least one tab");
        let render_dirty = self
            .active_tab()
            .map(|tab| tab.render_dirty.clone())
            .expect("workspace must always have at least one tab");

        let tab = Tab::new(
            number,
            cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
        )?;
        self.register_new_pane(tab.root_pane);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        Ok(self.active_tab)
    }

    pub fn close_tab(&mut self, idx: usize) -> bool {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return false;
        }
        let tab = self.tabs.remove(idx);
        for pane_id in tab.panes.keys() {
            self.unregister_pane(*pane_id);
        }
        self.renumber_tabs();
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if idx <= self.active_tab && self.active_tab > 0 {
            self.active_tab -= 1;
        }
        true
    }

    pub fn close_active_tab(&mut self) -> bool {
        self.close_tab(self.active_tab)
    }

    pub fn split_focused(
        &mut self,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
    ) -> std::io::Result<PaneId> {
        let new_id = self
            .active_tab_mut()
            .expect("workspace must always have at least one tab")
            .split_focused(
                direction,
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
            )?;
        self.register_new_pane(new_id);
        Ok(new_id)
    }

    /// Close the focused pane. Returns true if the workspace should close.
    pub fn close_focused(&mut self) -> bool {
        let pane_count = self
            .active_tab()
            .map(|tab| tab.layout.pane_count())
            .unwrap_or(0);
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            return tab_count <= 1 || self.close_active_tab_and_report();
        }

        if let Some((removed, runtime)) = self.active_tab_mut().and_then(Tab::close_focused) {
            self.unregister_pane(removed);
            if let Some(runtime) = runtime {
                runtime.shutdown(removed);
            }
        }
        false
    }

    /// Remove a specific pane from this workspace without terminating its runtime.
    /// Returns true if the workspace should close.
    pub fn remove_pane(&mut self, pane_id: PaneId) -> bool {
        let Some(tab_idx) = self.find_tab_index_for_pane(pane_id) else {
            return false;
        };
        let pane_count = self.tabs[tab_idx].layout.pane_count();
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            if tab_count <= 1 {
                return true;
            }
            self.tabs.remove(tab_idx);
            self.unregister_pane(pane_id);
            self.renumber_tabs();
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if tab_idx <= self.active_tab && self.active_tab > 0 {
                self.active_tab -= 1;
            }
            return false;
        }

        if let Some((removed, _)) = self.tabs[tab_idx].remove_pane(pane_id) {
            self.unregister_pane(removed);
        }
        false
    }

    pub fn public_pane_number(&self, pane_id: PaneId) -> Option<usize> {
        self.public_pane_numbers.get(&pane_id).copied()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn resolved_identity_cwd(&self) -> Option<PathBuf> {
        self.tabs
            .first()
            .and_then(Tab::root_cwd)
            .or_else(|| Some(self.identity_cwd.clone()))
    }

    pub fn display_name(&self) -> String {
        if let Some(name) = &self.custom_name {
            return name.clone();
        }

        self.resolved_identity_cwd()
            .map(|cwd| derive_label_from_cwd(&cwd))
            .unwrap_or_else(|| "workspace".into())
    }

    pub fn branch(&self) -> Option<String> {
        self.resolved_identity_cwd()
            .and_then(|cwd| git_branch(&cwd))
    }

    pub fn git_ahead_behind(&self) -> Option<(usize, usize)> {
        self.cached_git_ahead_behind
    }

    pub fn refresh_git_ahead_behind(&mut self) {
        self.cached_git_ahead_behind = self
            .resolved_identity_cwd()
            .and_then(|cwd| git_ahead_behind(&cwd));
    }

    pub fn aggregate_state(&self) -> (AgentState, bool) {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| (pane.state, pane.seen))
            .max_by_key(|(state, seen)| pane_attention_priority(*state, *seen))
            .unwrap_or((AgentState::Unknown, true))
    }

    pub fn has_working_pane(&self) -> bool {
        self.tabs.iter().any(Tab::has_working_pane)
    }

    pub fn pane_details(&self) -> Vec<PaneDetail> {
        let multi_tab = self.tabs.len() > 1;
        self.tabs
            .iter()
            .flat_map(Tab::pane_details)
            .map(|mut detail| {
                if multi_tab {
                    detail.label = format!("{}·{}", detail.tab_label, detail.agent_label);
                }
                detail
            })
            .collect()
    }

    pub fn focused_runtime(&self) -> Option<&PaneRuntime> {
        self.active_tab().and_then(Tab::focused_runtime)
    }

    pub fn find_tab_index_for_pane(&self, pane_id: PaneId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.panes.contains_key(&pane_id))
    }

    pub fn pane_state(&self, pane_id: PaneId) -> Option<&PaneState> {
        self.tabs.iter().find_map(|tab| tab.panes.get(&pane_id))
    }

    pub fn runtime(&self, pane_id: PaneId) -> Option<&PaneRuntime> {
        self.tabs.iter().find_map(|tab| tab.runtimes.get(&pane_id))
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.active_tab().map(|tab| tab.layout.focused())
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> bool {
        let tab_idx = match self.find_tab_index_for_pane(pane_id) {
            Some(idx) => idx,
            None => return false,
        };
        let pane_count = self.tabs[tab_idx].layout.pane_count();
        let tab_count = self.tabs.len();
        if pane_count <= 1 {
            if tab_count <= 1 {
                return true;
            }
            self.tabs.remove(tab_idx);
            self.unregister_pane(pane_id);
            self.renumber_tabs();
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            } else if tab_idx <= self.active_tab && self.active_tab > 0 {
                self.active_tab -= 1;
            }
            return false;
        }

        if let Some((removed, runtime)) = self.tabs[tab_idx].close_pane(pane_id) {
            self.unregister_pane(removed);
            if let Some(runtime) = runtime {
                runtime.shutdown(removed);
            }
        }
        false
    }

    fn register_new_pane(&mut self, pane_id: PaneId) {
        self.public_pane_numbers
            .insert(pane_id, self.next_public_pane_number);
        self.next_public_pane_number += 1;
    }

    fn unregister_pane(&mut self, pane_id: PaneId) {
        if let Some(removed_number) = self.public_pane_numbers.remove(&pane_id) {
            for number in self.public_pane_numbers.values_mut() {
                if *number > removed_number {
                    *number -= 1;
                }
            }
            self.next_public_pane_number = self.public_pane_numbers.len() + 1;
        }
    }

    fn renumber_tabs(&mut self) {
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            tab.number = idx + 1;
        }
    }

    fn close_active_tab_and_report(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return true;
        }
        self.close_active_tab();
        false
    }
}

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub tab_label: String,
    pub label: String,
    pub agent_label: String,
    #[allow(dead_code)]
    pub agent: Option<Agent>,
    pub state: AgentState,
    pub seen: bool,
}

fn pane_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
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

pub fn derive_label_from_cwd(cwd: &Path) -> String {
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

pub fn git_branch(cwd: &Path) -> Option<String> {
    git_repo_root(cwd)?;

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let branch = stdout.trim();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch.to_string())
    }
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

fn git_ahead_behind(cwd: &Path) -> Option<(usize, usize)> {
    git_repo_root(cwd)?;

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_ahead_behind_output(&stdout)
}

fn parse_git_ahead_behind_output(stdout: &str) -> Option<(usize, usize)> {
    let mut parts = stdout.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[cfg(test)]
impl Workspace {
    pub fn test_new(name: &str) -> Self {
        let (events, _) = mpsc::channel(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let identity_cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let (layout, root_id) = TileLayout::new();
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let mut pane_cwds = HashMap::new();
        pane_cwds.insert(root_id, identity_cwd.clone());
        let tab = Tab {
            custom_name: None,
            number: 1,
            root_pane: root_id,
            layout,
            panes,
            pane_cwds,
            runtimes: HashMap::new(),
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        };
        let mut public_pane_numbers = HashMap::new();
        public_pane_numbers.insert(tab.root_pane, 1);
        Self {
            id: generate_workspace_id(),
            custom_name: Some(name.to_string()),
            identity_cwd,
            cached_git_ahead_behind: None,
            public_pane_numbers,
            next_public_pane_number: 2,
            tabs: vec![tab],
            active_tab: 0,
        }
    }

    pub fn test_split(&mut self, direction: Direction) -> PaneId {
        let tab = self.active_tab_mut().expect("workspace must have tab");
        let new_id = tab.layout.split_focused(direction);
        tab.panes.insert(new_id, PaneState::new());
        let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        tab.pane_cwds.insert(new_id, cwd);
        self.register_new_pane(new_id);
        new_id
    }

    pub fn test_add_tab(&mut self, name: Option<&str>) -> usize {
        let (events, _) = mpsc::channel(64);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let (layout, root_id) = TileLayout::new();
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new());
        let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
        let mut pane_cwds = HashMap::new();
        pane_cwds.insert(root_id, cwd);
        let tab = Tab {
            custom_name: name.map(str::to_string),
            number: self.tabs.len() + 1,
            root_pane: root_id,
            layout,
            panes,
            pane_cwds,
            runtimes: HashMap::new(),
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        };
        self.register_new_pane(root_id);
        self.tabs.push(tab);
        self.tabs.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};

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
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        ws.tabs[0].panes.get_mut(&root_id).unwrap().state = AgentState::Idle;
        ws.tabs[0].panes.get_mut(&id2).unwrap().state = AgentState::Working;

        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.state = AgentState::Idle;
        root.seen = false;
        ws.tabs[0].panes.get_mut(&id2).unwrap().state = AgentState::Working;

        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Idle);
        assert!(!seen);
    }

    #[test]
    fn workspace_identity_follows_first_tab_root_pane_cwd() {
        let mut ws = Workspace::test_new("ignored");
        ws.custom_name = None;
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0]
            .pane_cwds
            .insert(root_pane, PathBuf::from("/tmp/pion"));

        assert_eq!(ws.display_name(), "pion");
        assert_eq!(ws.resolved_identity_cwd(), Some(PathBuf::from("/tmp/pion")));
    }

    #[test]
    fn pane_details_hide_plain_shells() {
        let mut ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0].panes.get_mut(&root_pane).unwrap().detected_agent = Some(Agent::Pi);
        ws.test_split(Direction::Horizontal);

        let details = ws.pane_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].label, "pi");
    }

    #[test]
    fn pane_details_include_tab_context_when_workspace_has_multiple_tabs() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0].panes.get_mut(&root_pane).unwrap().detected_agent = Some(Agent::Pi);

        let tab_idx = ws.test_add_tab(Some("logs"));
        let second_root_pane = ws.tabs[tab_idx].root_pane;
        ws.tabs[tab_idx]
            .panes
            .get_mut(&second_root_pane)
            .unwrap()
            .detected_agent = Some(Agent::Claude);

        let details = ws.pane_details();
        assert_eq!(details.len(), 2);
        assert!(details.iter().any(|detail| detail.label == "main·pi"));
        assert!(details.iter().any(|detail| detail.label == "logs·claude"));
    }
}
