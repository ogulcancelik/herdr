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

    /// Like parse_pane_id, but with the API caller's process ancestry as the
    /// authority when a peer pid is available. An env-baked HERDR_PANE_ID can
    /// be stale in two ways: dangling (no such pane — heal by ancestry) or,
    /// worse, colliding with a *different* live pane after pane renumbering —
    /// in which case trusting it silently delivers reports to the wrong pane.
    /// So a parsed claim is only accepted once the caller is verified to be a
    /// descendant of the claimed pane's child process.
    pub(super) fn parse_pane_id_or_peer(
        &mut self,
        id: &str,
        peer_pid: Option<u32>,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let parsed = self.parse_pane_id(id);
        let Some(peer) = peer_pid else {
            return parsed;
        };
        let chain = peer_ancestor_chain(peer);
        self.reconcile_pane_claim(id, parsed, &chain)
    }

    /// Decide between a parsed pane claim and the caller's process ancestry.
    /// Ancestry wins on a positive mismatch; the claim survives only when it
    /// is verified or when ancestry yields no evidence at all (re-parented or
    /// daemonized callers).
    fn reconcile_pane_claim(
        &mut self,
        id: &str,
        parsed: Option<(usize, crate::layout::PaneId)>,
        chain: &[u32],
    ) -> Option<(usize, crate::layout::PaneId)> {
        if let Some((_, pane_id)) = parsed {
            if self
                .pane_child_pid(pane_id)
                .is_some_and(|child| chain.contains(&child))
            {
                return parsed;
            }
        }
        let resolved = self.resolve_pane_in_chain(chain);
        let Some((ws_idx, pane_id)) = resolved else {
            // No ancestry evidence: keep the claim rather than dropping the
            // report (the claimed pane may simply have no live child pid).
            return parsed;
        };
        if parsed.is_some_and(|(_, claimed)| claimed == pane_id) {
            return resolved;
        }
        tracing::info!(
            stale_id = id,
            ws_idx,
            resolved = pane_id.raw(),
            claimed_live_pane = parsed.is_some(),
            "resolved pane by process ancestry over claimed id"
        );
        // Memoize: the next report from this environment takes the alias fast
        // path instead of re-walking the process tree. Only safe when the raw
        // id is dangling or already alias-routed — aliasing a raw id that
        // still names a live pane directly would shadow that pane's own
        // truthful reports.
        if let Some(stale_raw) = id
            .strip_prefix("p_")
            .and_then(|rest| rest.parse::<u32>().ok())
        {
            let raw_names_live_pane = self
                .find_pane(crate::layout::PaneId::from_raw(stale_raw))
                .is_some();
            let already_aliased = self.state.pane_id_aliases.contains_key(&stale_raw);
            if stale_raw != pane_id.raw() && (!raw_names_live_pane || already_aliased) {
                self.state.pane_id_aliases.insert(stale_raw, pane_id);
            }
        }
        resolved
    }

    /// PID of the pane's direct child process, when alive.
    fn pane_child_pid(&self, pane_id: crate::layout::PaneId) -> Option<u32> {
        #[cfg(test)]
        if let Some(pid) = self.test_pane_child_pids.get(&pane_id) {
            return Some(*pid);
        }
        let (_ws_idx, pane) = self.find_pane(pane_id)?;
        self.terminal_runtimes
            .get(&pane.attached_terminal_id)
            .and_then(crate::terminal::TerminalRuntime::child_pid)
    }

    fn resolve_pane_in_chain(&self, chain: &[u32]) -> Option<(usize, crate::layout::PaneId)> {
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for pane_id in tab.panes.keys() {
                    if self
                        .pane_child_pid(*pane_id)
                        .is_some_and(|child| chain.contains(&child))
                    {
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

/// Collect the peer's ancestor chain (bounded; refreshes one pid at a time).
fn peer_ancestor_chain(peer: u32) -> Vec<u32> {
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
    ancestors
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

    /// Two live panes with known child pids; the caller's ancestor chain
    /// places it in pane B while its env claims pane A.
    fn two_pane_app() -> (App, crate::layout::PaneId, crate::layout::PaneId) {
        let mut app = test_app();
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("a"),
            crate::workspace::Workspace::test_new("b"),
        ];
        let pane_a = app.state.workspaces[0].tabs[0].root_pane;
        let pane_b = app.state.workspaces[1].tabs[0].root_pane;
        app.test_pane_child_pids.insert(pane_a, 11_111);
        app.test_pane_child_pids.insert(pane_b, 22_222);
        (app, pane_a, pane_b)
    }

    #[test]
    fn colliding_stale_id_is_overridden_by_process_ancestry() {
        let (mut app, pane_a, pane_b) = two_pane_app();
        let id = format!("p_{}", pane_a.raw());
        let claimed = app.parse_pane_id(&id);
        assert_eq!(claimed.map(|(_, p)| p), Some(pane_a));

        // hook script (33_333) -> agent process = pane B's child (22_222)
        let resolved = app.reconcile_pane_claim(&id, claimed, &[33_333, 22_222]);
        assert_eq!(resolved.map(|(_, p)| p), Some(pane_b));
        // Never alias a raw id that still names a live pane: it would shadow
        // pane A's own truthful reports.
        assert!(!app.state.pane_id_aliases.contains_key(&pane_a.raw()));
    }

    #[test]
    fn truthful_id_is_verified_by_the_ancestry_fast_path() {
        let (mut app, pane_a, _pane_b) = two_pane_app();
        let id = format!("p_{}", pane_a.raw());
        let claimed = app.parse_pane_id(&id);

        let resolved = app.reconcile_pane_claim(&id, claimed, &[33_333, 11_111]);
        assert_eq!(resolved.map(|(_, p)| p), Some(pane_a));
        assert!(app.state.pane_id_aliases.is_empty());
    }

    #[test]
    fn claim_survives_when_ancestry_yields_no_evidence() {
        // Re-parented/daemonized callers produce a chain that matches no
        // pane; the claimed pane must keep receiving reports.
        let (mut app, pane_a, _pane_b) = two_pane_app();
        let id = format!("p_{}", pane_a.raw());
        let claimed = app.parse_pane_id(&id);

        let resolved = app.reconcile_pane_claim(&id, claimed, &[999_999]);
        assert_eq!(resolved.map(|(_, p)| p), Some(pane_a));
    }

    #[test]
    fn dangling_id_resolves_by_ancestry_and_memoizes_an_alias() {
        let (mut app, _pane_a, pane_b) = two_pane_app();

        let resolved = app.reconcile_pane_claim("p_999999", None, &[33_333, 22_222]);
        assert_eq!(resolved.map(|(_, p)| p), Some(pane_b));
        assert_eq!(app.state.pane_id_aliases.get(&999_999), Some(&pane_b));
    }
}
