use super::*;

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

    fn temp_test_dir(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-workspace-tests-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

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
    fn git_branch_reads_head_from_standard_repo() {
        let root = temp_test_dir("standard-repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        assert_eq!(git_branch(&root).as_deref(), Some("main"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_branch_reads_head_from_worktree_gitdir_file() {
        let root = temp_test_dir("worktree");
        let worktree_git_dir = root.join(".bare/worktrees/feature");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        std::fs::write(root.join(".git"), "gitdir: .bare/worktrees/feature\n").unwrap();
        std::fs::write(worktree_git_dir.join("HEAD"), "ref: refs/heads/feature\n").unwrap();

        assert_eq!(git_branch(&root).as_deref(), Some("feature"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_branch_returns_none_for_detached_head() {
        let root = temp_test_dir("detached-head");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "3e1b9a8d\n").unwrap();

        assert_eq!(git_branch(&root), None);

        std::fs::remove_dir_all(root).unwrap();
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
    fn moving_tab_keeps_active_identity_and_renumbers_auto_tabs() {
        let mut ws = Workspace::test_new("test");
        let moved_root = ws.tabs[0].root_pane;
        ws.test_add_tab(Some("foo"));
        let final_auto_idx = ws.test_add_tab(None);
        let active_root = ws.tabs[final_auto_idx].root_pane;
        ws.switch_tab(final_auto_idx);

        assert!(ws.move_tab(0, ws.tabs.len()));

        let labels: Vec<_> = ws.tabs.iter().map(|tab| tab.display_name()).collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(ws.tabs[0].custom_name.as_deref(), Some("foo"));
        assert!(ws.tabs[1].custom_name.is_none());
        assert!(ws.tabs[2].custom_name.is_none());
        assert_eq!(ws.tabs[2].root_pane, moved_root);
        assert_eq!(ws.tabs[ws.active_tab].root_pane, active_root);
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

    #[test]
    fn pane_details_include_hook_reported_unknown_agents() {
        let mut ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0]
            .panes
            .get_mut(&root_pane)
            .unwrap()
            .set_hook_authority(
                "custom:hermes".into(),
                "hermes".into(),
                AgentState::Working,
                None,
            );

        let details = ws.pane_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].agent_label, "hermes");
        assert_eq!(details[0].agent, None);
    }
}
