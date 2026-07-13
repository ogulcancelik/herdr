use crate::config::{
    AgentSidebarToken, AgentsSidebarConfig, SpaceSidebarToken, SpacesSidebarConfig,
};

use super::AgentPanelEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedToken {
    StateIcon,
    StateText(String),
    Workspace(String),
    Tab(String),
    Pane(String),
    Agent(String),
    TerminalTitle(String),
    Branch(String),
    GitStatus { ahead: usize, behind: usize },
    Custom(String),
}

pub(super) fn agent_rows(
    config: &AgentsSidebarConfig,
    entry: &AgentPanelEntry,
    state_text: &str,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows_for_agent(entry.agent)
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|token| match token {
                    AgentSidebarToken::StateIcon => Some(ResolvedToken::StateIcon),
                    AgentSidebarToken::StateText => {
                        Some(ResolvedToken::StateText(state_text.to_string()))
                    }
                    AgentSidebarToken::Workspace => {
                        Some(ResolvedToken::Workspace(entry.primary_label.clone()))
                    }
                    AgentSidebarToken::Tab => {
                        entry.primary_tab_label.clone().map(ResolvedToken::Tab)
                    }
                    AgentSidebarToken::Pane => entry.pane_label.clone().map(ResolvedToken::Pane),
                    AgentSidebarToken::Agent => entry.agent_label.clone().map(ResolvedToken::Agent),
                    AgentSidebarToken::TerminalTitle => entry
                        .terminal_title
                        .clone()
                        .map(ResolvedToken::TerminalTitle),
                    AgentSidebarToken::TerminalTitleStripped => entry
                        .terminal_title_stripped
                        .clone()
                        .map(ResolvedToken::TerminalTitle),
                    AgentSidebarToken::Custom(name) => {
                        entry.tokens.get(name).cloned().map(ResolvedToken::Custom)
                    }
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) struct SpaceTokenContext<'a> {
    pub workspace: &'a str,
    pub branch: Option<&'a str>,
    pub state_text: &'a str,
    pub ahead_behind: Option<(usize, usize)>,
    pub tokens: &'a std::collections::HashMap<String, String>,
    pub suppress_git_details: bool,
}

pub(super) fn space_rows(
    config: &SpacesSidebarConfig,
    context: SpaceTokenContext<'_>,
) -> Vec<Vec<ResolvedToken>> {
    config
        .rows
        .iter()
        .filter_map(|row| {
            let resolved = row
                .iter()
                .filter_map(|token| match token {
                    SpaceSidebarToken::StateIcon => Some(ResolvedToken::StateIcon),
                    SpaceSidebarToken::StateText => {
                        Some(ResolvedToken::StateText(context.state_text.to_string()))
                    }
                    SpaceSidebarToken::Workspace => {
                        Some(ResolvedToken::Workspace(context.workspace.to_string()))
                    }
                    SpaceSidebarToken::Branch if !context.suppress_git_details => context
                        .branch
                        .map(|branch| ResolvedToken::Branch(branch.to_string())),
                    SpaceSidebarToken::Branch => None,
                    SpaceSidebarToken::GitStatus if !context.suppress_git_details => context
                        .ahead_behind
                        .filter(|(ahead, behind)| *ahead > 0 || *behind > 0)
                        .map(|(ahead, behind)| ResolvedToken::GitStatus { ahead, behind }),
                    SpaceSidebarToken::GitStatus => None,
                    SpaceSidebarToken::Custom(name) => {
                        context.tokens.get(name).cloned().map(ResolvedToken::Custom)
                    }
                })
                .collect::<Vec<_>>();
            (!resolved.is_empty()).then_some(resolved)
        })
        .collect()
}

pub(super) fn separator(previous: &ResolvedToken, current: &ResolvedToken) -> &'static str {
    if matches!(previous, ResolvedToken::StateIcon)
        || matches!(current, ResolvedToken::GitStatus { .. })
    {
        " "
    } else {
        " · "
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentState;

    fn entry() -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            primary_label: "repo".into(),
            primary_tab_label: None,
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: Some("pi".into()),
            agent: Some(crate::detect::Agent::Pi),
            state: AgentState::Working,
            seen: true,
            last_agent_state_change_seq: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn missing_custom_tokens_elide_rows_and_separators() {
        let entry = entry();
        let config = AgentsSidebarConfig {
            rows: vec![
                vec![
                    AgentSidebarToken::StateIcon,
                    AgentSidebarToken::Custom("missing".into()),
                ],
                vec![AgentSidebarToken::Custom("missing".into())],
                vec![AgentSidebarToken::Agent],
            ],
            rows_by_agent: Default::default(),
        };

        let rows = agent_rows(&config, &entry, "working");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec![ResolvedToken::StateIcon]);
        assert_eq!(rows[1], vec![ResolvedToken::Agent("pi".into())]);
    }

    #[test]
    fn state_text_and_arbitrary_values_are_independent_tokens() {
        let mut entry = entry();
        entry
            .tokens
            .insert("summary".into(), "reviewing auth".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::StateText,
                AgentSidebarToken::Custom("summary".into()),
            ]],
            rows_by_agent: Default::default(),
        };

        assert_eq!(
            agent_rows(&config, &entry, "deep in the mines"),
            vec![vec![
                ResolvedToken::StateText("deep in the mines".into()),
                ResolvedToken::Custom("reviewing auth".into()),
            ]]
        );
    }

    #[test]
    fn terminal_title_builtins_are_distinct_from_custom_tokens() {
        let mut entry = entry();
        entry.terminal_title = Some("⠋ raw title".into());
        entry.terminal_title_stripped = Some("raw title".into());
        entry
            .tokens
            .insert("terminal_title".into(), "custom title".into());
        let config = AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::TerminalTitle,
                AgentSidebarToken::TerminalTitleStripped,
                AgentSidebarToken::Custom("terminal_title".into()),
            ]],
            rows_by_agent: Default::default(),
        };

        assert_eq!(
            agent_rows(&config, &entry, "working"),
            vec![vec![
                ResolvedToken::TerminalTitle("⠋ raw title".into()),
                ResolvedToken::TerminalTitle("raw title".into()),
                ResolvedToken::Custom("custom title".into()),
            ]]
        );
    }

    #[test]
    fn known_agent_override_replaces_default_rows() {
        let mut config = AgentsSidebarConfig {
            rows: vec![vec![AgentSidebarToken::Workspace]],
            rows_by_agent: Default::default(),
        };
        config
            .rows_by_agent
            .insert("pi".into(), vec![vec![AgentSidebarToken::Agent]]);
        let mut pi = entry();
        pi.agent_label = Some("renamed pi".into());

        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::Agent("renamed pi".into())]]
        );

        pi.agent = None;
        assert_eq!(
            agent_rows(&config, &pi, "working"),
            vec![vec![ResolvedToken::Workspace("repo".into())]]
        );
    }

    #[test]
    fn grouped_children_suppress_all_builtin_git_details() {
        let config = SpacesSidebarConfig::default();

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "feature",
                    branch: Some("worktree/feature"),
                    state_text: "idle",
                    ahead_behind: Some((2, 1)),
                    tokens: &std::collections::HashMap::new(),
                    suppress_git_details: true,
                },
            ),
            vec![vec![
                ResolvedToken::StateIcon,
                ResolvedToken::Workspace("feature".into()),
            ]]
        );
    }

    #[test]
    fn workspace_custom_token_can_replace_git_specific_details() {
        let tokens = std::collections::HashMap::from([("jj_status".into(), "2 changes".into())]);
        let config = SpacesSidebarConfig {
            rows: vec![vec![SpaceSidebarToken::Custom("jj_status".into())]],
        };

        assert_eq!(
            space_rows(
                &config,
                SpaceTokenContext {
                    workspace: "repo",
                    branch: None,
                    state_text: "idle",
                    ahead_behind: None,
                    tokens: &tokens,
                    suppress_git_details: false,
                },
            ),
            vec![vec![ResolvedToken::Custom("2 changes".into())]]
        );
    }
}
