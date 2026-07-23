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
        let tab_number = ws.public_tab_number(tab_idx)?;
        Some(crate::workspace::public_tab_id_for_number(
            &ws.id, tab_number,
        ))
    }

    pub(super) fn public_pane_id(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        Some(crate::workspace::public_pane_id_for_number(
            &ws.id,
            pane_number,
        ))
    }

    pub(super) fn pane_launch_env(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        extra_env: Vec<(String, String)>,
    ) -> Option<crate::pane::PaneLaunchEnv> {
        let workspace_id = self.public_workspace_id(ws_idx);
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let tab_id = self.public_tab_id(ws_idx, tab_idx)?;
        let pane_id = self.public_pane_id(ws_idx, pane_id)?;
        Some(
            crate::pane::PaneLaunchEnv::from_extra(extra_env).with_identity(
                workspace_id,
                tab_id,
                pane_id,
            ),
        )
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
        let tab_idx = if let Some(encoded) = tab_raw.strip_prefix('t') {
            let tab_number = crate::workspace::decode_public_number(encoded)?;
            self.state
                .workspaces
                .get(ws_idx)?
                .tabs
                .iter()
                .position(|tab| tab.number == tab_number)?
        } else {
            tab_raw.parse::<usize>().ok()?.checked_sub(1)?
        };
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

    pub(super) fn parse_pane_id(&self, id: &str) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(alias) = self.state.public_pane_id_aliases.get(id).copied() {
            return self.find_pane(alias).map(|(ws_idx, _)| (ws_idx, alias));
        }

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

        if let Some((ws_raw, pane_number_raw)) = id.rsplit_once(":p") {
            let ws_idx = self.parse_workspace_id(ws_raw)?;
            let pane_number = crate::workspace::decode_public_number(pane_number_raw)?;
            let ws = self.state.workspaces.get(ws_idx)?;
            let pane_id = ws
                .public_pane_numbers
                .iter()
                .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
            return Some((ws_idx, pane_id));
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

    pub(crate) fn parse_current_public_pane_id(
        &self,
        id: &str,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let (ws_idx, pane_id) = self.parse_pane_id(id)?;
        (self.public_pane_id(ws_idx, pane_id).as_deref() == Some(id)).then_some((ws_idx, pane_id))
    }
}
