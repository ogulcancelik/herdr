use super::App;

impl App {
    pub(crate) fn find_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<(usize, &crate::pane::PaneState)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| ws.pane_state(pane_id).map(|pane| (ws_idx, pane)))
    }

    pub(super) fn public_workspace_id(&self, ws_idx: usize) -> String {
        self.state.workspaces[ws_idx].id.clone()
    }

    pub(super) fn public_tab_id(&self, ws_idx: usize, tab_idx: usize) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        ws.tabs.get(tab_idx)?;
        Some(format!("{}:{}", ws.id, tab_idx + 1))
    }

    pub(super) fn public_pane_id(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        Some(format!("{}-{pane_number}", ws.id))
    }

    pub(super) fn parse_workspace_id(&self, id: &str) -> Option<usize> {
        self.state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == id)
            .or_else(|| id.strip_prefix("w_")?.parse::<usize>().ok()?.checked_sub(1))
            .or_else(|| id.parse::<usize>().ok()?.checked_sub(1))
    }

    pub(super) fn parse_tab_id(&self, id: &str) -> Option<(usize, usize)> {
        if let Some(rest) = id.strip_prefix("t_") {
            let (ws_raw, tab_raw) = rest.rsplit_once('_')?;
            let ws_idx = self.parse_workspace_id(ws_raw)?;
            let tab_idx = tab_raw.parse::<usize>().ok()?.checked_sub(1)?;
            self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
            return Some((ws_idx, tab_idx));
        }

        let (ws_raw, tab_raw) = id.rsplit_once(':')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let tab_idx = tab_raw.parse::<usize>().ok()?.checked_sub(1)?;
        self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        Some((ws_idx, tab_idx))
    }

    fn resolve_raw_pane_id(&self, raw: u32) -> Option<crate::layout::PaneId> {
        if let Some(alias) = self.state.pane_id_aliases.get(&raw).copied() {
            return self.find_pane(alias).map(|_| alias);
        }
        let pane_id = crate::layout::PaneId::from_raw(raw);
        if self.find_pane(pane_id).is_some() {
            return Some(pane_id);
        }
        None
    }

    /// Like parse_pane_id, but when the id is stale (e.g. HERDR_PANE_ID baked
    /// into a pane environment that predates alias tracking) falls back to
    /// resolving the API caller by process ancestry: peer pid -> parents ->
    /// a pane's direct child pid.
    pub(super) fn parse_pane_id_or_peer(
        &mut self,
        id: &str,
        peer_pid: Option<u32>,
    ) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(found) = self.parse_pane_id(id) {
            return Some(found);
        }
        let peer = peer_pid?;
        let resolved = self.resolve_pane_by_process_ancestry(peer);
        if let Some((ws_idx, pane_id)) = resolved {
            tracing::info!(
                stale_id = id,
                peer_pid = peer,
                ws_idx,
                resolved = pane_id.raw(),
                "resolved stale pane id via process ancestry"
            );
            // Memoize: the next report from this environment takes the alias
            // fast path instead of re-walking the process tree. The alias map
            // already handles shadowing, persistence, and handoff chaining.
            if let Some(stale_raw) = id
                .strip_prefix("p_")
                .and_then(|rest| rest.parse::<u32>().ok())
            {
                if stale_raw != pane_id.raw() {
                    self.state.pane_id_aliases.insert(stale_raw, pane_id);
                }
            }
        }
        resolved
    }

    fn resolve_pane_by_process_ancestry(
        &self,
        peer: u32,
    ) -> Option<(usize, crate::layout::PaneId)> {
        // Collect the peer's ancestor chain (bounded; refreshes one pid at a time).
        let mut system = sysinfo::System::new();
        let mut ancestors = Vec::with_capacity(16);
        let mut current = peer;
        for _ in 0..16 {
            ancestors.push(current);
            system.refresh_processes(
                sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(current)]),
                true,
            );
            let Some(parent) = system
                .process(sysinfo::Pid::from_u32(current))
                .and_then(|process| process.parent())
            else {
                break;
            };
            current = parent.as_u32();
            if current <= 1 {
                break;
            }
        }

        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(child) = self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .and_then(crate::terminal::TerminalRuntime::child_pid)
                    else {
                        continue;
                    };
                    if ancestors.contains(&child) {
                        return Some((ws_idx, *pane_id));
                    }
                }
            }
        }
        None
    }

    pub(super) fn parse_pane_id(&self, id: &str) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(rest) = id.strip_prefix("p_") {
            if let Some((ws_raw, pane_raw)) = rest.rsplit_once('_') {
                let ws_idx = self.parse_workspace_id(ws_raw)?;
                let pane_id = self.resolve_raw_pane_id(pane_raw.parse::<u32>().ok()?)?;
                self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
                return Some((ws_idx, pane_id));
            }

            let pane_id = self.resolve_raw_pane_id(rest.parse::<u32>().ok()?)?;
            return self.find_pane(pane_id).map(|(ws_idx, _)| (ws_idx, pane_id));
        }

        let (ws_raw, pane_number_raw) = id.rsplit_once('-')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let pane_number = pane_number_raw.parse::<usize>().ok()?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws
            .public_pane_numbers
            .iter()
            .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
        Some((ws_idx, pane_id))
    }
}
#[cfg(test)]
mod tests {
    use super::super::App;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn alias_gc_drops_entries_for_dead_target_panes() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("main")];
        let live = app.state.workspaces[0].tabs[0].root_pane;
        let dead = crate::layout::PaneId::from_raw(9_999);

        app.state.pane_id_aliases.insert(1_000, live);
        app.state.pane_id_aliases.insert(1_001, dead);

        app.state
            .remove_alias_shadowed_by_new_pane(crate::layout::PaneId::from_raw(7_777));

        assert_eq!(app.state.pane_id_aliases.get(&1_000), Some(&live));
        assert_eq!(app.state.pane_id_aliases.get(&1_001), None);
    }

    #[test]
    fn memoized_alias_takes_the_fast_path_after_ancestry_resolution() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("main")];
        let pane = app.state.workspaces[0].tabs[0].root_pane;

        // Simulate the memoization a successful ancestry walk performs.
        app.state.pane_id_aliases.insert(123_456, pane);

        // The stale id now resolves through parse_pane_id without any peer.
        let resolved = app.parse_pane_id_or_peer("p_123456", None);
        assert_eq!(resolved.map(|(_, id)| id), Some(pane));
    }
}
