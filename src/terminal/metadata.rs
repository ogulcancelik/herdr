use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::detect::AgentState;

use super::{TerminalState, TerminalStateMutation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMetadata {
    pub source: String,
    pub agent_label: Option<String>,
    pub applies_to_source: Option<String>,
    pub title: Option<String>,
    pub display_agent: Option<String>,
    pub custom_status: Option<String>,
    pub state_labels: HashMap<String, String>,
    pub reported_at: Instant,
    title_reported_at: Option<Instant>,
    display_agent_reported_at: Option<Instant>,
    custom_status_reported_at: Option<Instant>,
    state_label_reported_at: HashMap<String, Instant>,
    pub ttl: Option<Duration>,
    expiry_event_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMetadataReport {
    pub source: String,
    pub agent_label: Option<String>,
    pub applies_to_source: Option<String>,
    pub title: Option<String>,
    pub display_agent: Option<String>,
    pub custom_status: Option<String>,
    pub state_labels: HashMap<String, String>,
    pub clear_title: bool,
    pub clear_display_agent: bool,
    pub clear_custom_status: bool,
    pub clear_state_labels: bool,
    pub ttl: Option<Duration>,
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePresentation {
    pub title: Option<String>,
    pub display_agent: Option<String>,
    pub custom_status: Option<String>,
    pub state_labels: HashMap<String, String>,
}

impl EffectivePresentation {
    fn empty() -> Self {
        Self {
            title: None,
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
        }
    }
}

impl TerminalState {
    fn accept_metadata_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        let Some(seq) = seq else {
            return true;
        };

        if self
            .metadata_report_sequences
            .get(source)
            .is_some_and(|last_seq| seq <= *last_seq)
        {
            return false;
        }

        self.metadata_report_sequences
            .insert(source.to_string(), seq);
        true
    }

    pub fn set_agent_metadata(
        &mut self,
        report: AgentMetadataReport,
    ) -> Option<TerminalStateMutation> {
        if !self.accept_metadata_report(&report.source, report.seq) {
            return None;
        }

        let now = Instant::now();
        if self
            .agent_metadata
            .get(&report.source)
            .is_some_and(|metadata| self.agent_metadata_is_expired(metadata, now))
        {
            self.agent_metadata.remove(&report.source);
        }
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let has_set_fields = report.title.is_some()
            || report.display_agent.is_some()
            || report.custom_status.is_some()
            || !report.state_labels.is_empty();

        let report_source = report.source.clone();
        let report_has_ttl = report.ttl.is_some();

        if report.clear_title
            || report.clear_display_agent
            || report.clear_custom_status
            || report.clear_state_labels
        {
            let metadata = self
                .agent_metadata
                .entry(report.source.clone())
                .or_insert_with(|| AgentMetadata {
                    source: report.source.clone(),
                    agent_label: report.agent_label.clone(),
                    applies_to_source: report.applies_to_source.clone(),
                    title: None,
                    display_agent: None,
                    custom_status: None,
                    state_labels: HashMap::new(),
                    reported_at: now,
                    title_reported_at: None,
                    display_agent_reported_at: None,
                    custom_status_reported_at: None,
                    state_label_reported_at: HashMap::new(),
                    ttl: report.ttl,
                    expiry_event_pending: false,
                });
            if report.clear_title {
                metadata.title = None;
                metadata.title_reported_at = None;
            }
            if report.clear_display_agent {
                metadata.display_agent = None;
                metadata.display_agent_reported_at = None;
            }
            if report.clear_custom_status {
                metadata.custom_status = None;
                metadata.custom_status_reported_at = None;
            }
            if report.clear_state_labels {
                metadata.state_labels.clear();
                metadata.state_label_reported_at.clear();
            }
            if let Some(agent_label) = report.agent_label {
                metadata.agent_label = Some(agent_label);
            }
            if let Some(applies_to_source) = report.applies_to_source {
                metadata.applies_to_source = Some(applies_to_source);
            }
            if let Some(title) = report.title {
                metadata.title = Some(title);
                metadata.title_reported_at = Some(now);
            }
            if let Some(display_agent) = report.display_agent {
                metadata.display_agent = Some(display_agent);
                metadata.display_agent_reported_at = Some(now);
            }
            if let Some(custom_status) = report.custom_status {
                metadata.custom_status = Some(custom_status);
                metadata.custom_status_reported_at = Some(now);
            }
            for (state, label) in report.state_labels {
                metadata.state_labels.insert(state.clone(), label);
                metadata.state_label_reported_at.insert(state, now);
            }
            if has_set_fields || report.ttl.is_some() {
                metadata.reported_at = now;
                metadata.ttl = report.ttl;
                metadata.expiry_event_pending = false;
            }
        } else {
            let title_reported_at = report.title.as_ref().map(|_| now);
            let display_agent_reported_at = report.display_agent.as_ref().map(|_| now);
            let custom_status_reported_at = report.custom_status.as_ref().map(|_| now);
            let state_label_reported_at = report
                .state_labels
                .keys()
                .map(|state| (state.clone(), now))
                .collect();
            self.agent_metadata.insert(
                report.source.clone(),
                AgentMetadata {
                    source: report.source,
                    agent_label: report.agent_label,
                    applies_to_source: report.applies_to_source,
                    title: report.title,
                    display_agent: report.display_agent,
                    custom_status: report.custom_status,
                    state_labels: report.state_labels,
                    reported_at: now,
                    title_reported_at,
                    display_agent_reported_at,
                    custom_status_reported_at,
                    state_label_reported_at,
                    ttl: report.ttl,
                    expiry_event_pending: false,
                },
            );
        }

        if report_has_ttl
            && self
                .agent_metadata
                .get(&report_source)
                .is_some_and(|metadata| self.agent_metadata_is_visible_ignoring_ttl(metadata))
        {
            if let Some(metadata) = self.agent_metadata.get_mut(&report_source) {
                metadata.expiry_event_pending = true;
            }
        }

        let effective_state_change = self.recompute_effective_state(
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            now,
        );

        if effective_state_change.is_some() {
            self.clear_expiry_pending_for_hidden_metadata();
        }

        Some(TerminalStateMutation {
            effective_state_change,
            session_ref_changed: false,
        })
    }
    pub fn effective_custom_status(&self) -> Option<String> {
        self.effective_presentation_for_state_at(self.state, Instant::now())
            .custom_status
    }

    pub fn effective_title(&self) -> Option<String> {
        self.effective_presentation_for_state_at(self.state, Instant::now())
            .title
    }

    pub fn effective_display_agent(&self) -> Option<String> {
        self.effective_presentation_for_state_at(self.state, Instant::now())
            .display_agent
    }

    pub fn effective_presentation(&self) -> EffectivePresentation {
        self.effective_presentation_for_state_at(self.state, Instant::now())
    }

    pub fn next_agent_metadata_expiry(&self) -> Option<Instant> {
        let now = Instant::now();
        self.agent_metadata
            .values()
            .filter(|metadata| self.agent_metadata_matches_guards(metadata))
            .filter_map(|metadata| self.agent_metadata_expiry(metadata))
            .filter(|deadline| {
                *deadline > now
                    || self.agent_metadata.values().any(|metadata| {
                        metadata.expiry_event_pending
                            && self.agent_metadata_matches_guards(metadata)
                            && self.agent_metadata_expiry(metadata) == Some(*deadline)
                    })
            })
            .min()
    }

    pub fn expire_agent_metadata_at(
        &mut self,
        scheduled_deadline: Instant,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        let (expired_sources, stale_sources): (Vec<_>, Vec<_>) = self
            .agent_metadata
            .iter()
            .filter_map(|(source, metadata)| {
                let deadline = self.agent_metadata_expiry(metadata)?;
                (deadline <= now).then_some((source.clone(), deadline))
            })
            .partition(|(_, deadline)| *deadline >= scheduled_deadline);
        let expired_sources: Vec<_> = expired_sources
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        let stale_sources: Vec<_> = stale_sources
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        for source in stale_sources {
            self.agent_metadata.remove(&source);
        }
        if expired_sources.is_empty() {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation =
            self.effective_presentation_for_state_at_ignoring_ttl(previous_state, now);
        for source in expired_sources {
            if let Some(metadata) = self.agent_metadata.get_mut(&source) {
                metadata.expiry_event_pending = false;
            }
            self.agent_metadata.remove(&source);
        }

        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: false,
        })
    }

    pub(super) fn effective_presentation_for_state_at(
        &self,
        state: AgentState,
        now: Instant,
    ) -> EffectivePresentation {
        self.effective_presentation_for_state_at_with_ttl(state, now, true)
    }

    fn effective_presentation_for_state_at_ignoring_ttl(
        &self,
        state: AgentState,
        now: Instant,
    ) -> EffectivePresentation {
        self.effective_presentation_for_state_at_with_ttl(state, now, false)
    }

    fn effective_presentation_for_state_at_with_ttl(
        &self,
        state: AgentState,
        now: Instant,
        enforce_ttl: bool,
    ) -> EffectivePresentation {
        let mut presentation = EffectivePresentation::empty();
        presentation.title = self.newest_metadata_title(now, enforce_ttl);
        presentation.display_agent = self.newest_metadata_display_agent(now, enforce_ttl);
        presentation.state_labels = self.effective_metadata_state_labels(now, enforce_ttl);
        presentation.custom_status =
            self.effective_custom_status_for_state_at_with_ttl(state, now, enforce_ttl);
        presentation
    }

    fn effective_custom_status_for_state_at_with_ttl(
        &self,
        state: AgentState,
        now: Instant,
        enforce_ttl: bool,
    ) -> Option<String> {
        if let Some(custom_status) = self.newest_metadata_custom_status(now, enforce_ttl) {
            return Some(custom_status);
        }

        if self.visible_blocker_overrides_hook()
            || self.visible_working_overrides_hook()
            || self.visible_idle_masks_hook_custom_status(state, now)
        {
            return None;
        }

        self.hook_authority
            .as_ref()
            .and_then(|authority| authority.custom_status.clone())
    }

    fn valid_agent_metadata(
        &self,
        now: Instant,
        enforce_ttl: bool,
    ) -> impl Iterator<Item = &AgentMetadata> {
        self.agent_metadata
            .values()
            .filter(move |metadata| self.agent_metadata_is_valid(metadata, now, enforce_ttl))
    }

    fn newest_metadata_title(&self, now: Instant, enforce_ttl: bool) -> Option<String> {
        self.valid_agent_metadata(now, enforce_ttl)
            .filter(|metadata| metadata.title.is_some())
            .max_by_key(|metadata| metadata.title_reported_at)
            .and_then(|metadata| metadata.title.clone())
    }

    fn newest_metadata_display_agent(&self, now: Instant, enforce_ttl: bool) -> Option<String> {
        self.valid_agent_metadata(now, enforce_ttl)
            .filter(|metadata| metadata.display_agent.is_some())
            .max_by_key(|metadata| metadata.display_agent_reported_at)
            .and_then(|metadata| metadata.display_agent.clone())
    }

    fn newest_metadata_custom_status(&self, now: Instant, enforce_ttl: bool) -> Option<String> {
        self.valid_agent_metadata(now, enforce_ttl)
            .filter(|metadata| metadata.custom_status.is_some())
            .max_by_key(|metadata| metadata.custom_status_reported_at)
            .and_then(|metadata| metadata.custom_status.clone())
    }

    fn effective_metadata_state_labels(
        &self,
        now: Instant,
        enforce_ttl: bool,
    ) -> HashMap<String, String> {
        let mut labels: Vec<_> = self
            .valid_agent_metadata(now, enforce_ttl)
            .flat_map(|metadata| {
                metadata.state_labels.iter().filter_map(|(state, label)| {
                    Some((
                        *metadata.state_label_reported_at.get(state)?,
                        state.clone(),
                        label.clone(),
                    ))
                })
            })
            .collect();
        labels.sort_by_key(|(reported_at, _, _)| *reported_at);
        labels
            .into_iter()
            .map(|(_, state, label)| (state, label))
            .collect()
    }

    fn agent_metadata_is_valid(
        &self,
        metadata: &AgentMetadata,
        now: Instant,
        enforce_ttl: bool,
    ) -> bool {
        if metadata.title.is_none()
            && metadata.display_agent.is_none()
            && metadata.custom_status.is_none()
            && metadata.state_labels.is_empty()
        {
            return false;
        }
        if enforce_ttl && self.agent_metadata_is_expired(metadata, now) {
            return false;
        }
        self.agent_metadata_matches_guards(metadata)
    }

    fn agent_metadata_is_visible_ignoring_ttl(&self, metadata: &AgentMetadata) -> bool {
        (metadata.title.is_some()
            || metadata.display_agent.is_some()
            || metadata.custom_status.is_some()
            || !metadata.state_labels.is_empty())
            && self.agent_metadata_matches_guards(metadata)
    }

    pub(super) fn clear_expiry_pending_for_hidden_metadata(&mut self) {
        let hidden_sources: Vec<_> = self
            .agent_metadata
            .iter()
            .filter(|(_, metadata)| {
                metadata.expiry_event_pending
                    && !self.agent_metadata_is_visible_ignoring_ttl(metadata)
            })
            .map(|(source, _)| source.clone())
            .collect();
        for source in hidden_sources {
            if let Some(metadata) = self.agent_metadata.get_mut(&source) {
                metadata.expiry_event_pending = false;
            }
        }
    }

    fn agent_metadata_is_expired(&self, metadata: &AgentMetadata, now: Instant) -> bool {
        self.agent_metadata_expiry(metadata)
            .is_some_and(|deadline| now >= deadline)
    }

    fn agent_metadata_expiry(&self, metadata: &AgentMetadata) -> Option<Instant> {
        metadata.ttl.map(|ttl| {
            metadata
                .reported_at
                .checked_add(ttl)
                .unwrap_or(metadata.reported_at)
        })
    }

    fn agent_metadata_matches_guards(&self, metadata: &AgentMetadata) -> bool {
        if metadata
            .agent_label
            .as_deref()
            .is_some_and(|agent| self.effective_agent_label() != Some(agent))
        {
            return false;
        }
        if metadata.applies_to_source.as_deref().is_some_and(|source| {
            self.hook_authority
                .as_ref()
                .is_none_or(|authority| authority.source != source)
        }) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::detect::Agent;
    use crate::terminal::TerminalId;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    fn set_metadata_custom_status(
        terminal: &mut TerminalState,
        source: &str,
        agent_label: Option<&str>,
        applies_to_source: Option<&str>,
        custom_status: Option<&str>,
        clear_custom_status: bool,
    ) -> Option<TerminalStateMutation> {
        terminal.set_agent_metadata(AgentMetadataReport {
            source: source.into(),
            agent_label: agent_label.map(str::to_string),
            applies_to_source: applies_to_source.map(str::to_string),
            title: None,
            display_agent: None,
            custom_status: custom_status.map(str::to_string),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        })
    }

    #[test]
    fn user_agent_metadata_overrides_hook_custom_status_without_changing_state() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_custom_status(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
        );

        let mutation = set_metadata_custom_status(
            &mut terminal,
            "user:claude-title",
            Some("claude"),
            Some("herdr:claude"),
            Some("refactor auth"),
            false,
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("refactor auth")
        );
        let change = mutation.unwrap().effective_state_change.unwrap();
        assert_eq!(change.previous_state, AgentState::Working);
        assert_eq!(change.state, AgentState::Working);
        assert_eq!(
            change.previous_presentation.custom_status.as_deref(),
            Some("thinking")
        );
        assert_eq!(
            change.presentation.custom_status.as_deref(),
            Some("refactor auth")
        );
    }

    #[test]
    fn user_agent_metadata_requires_matching_lifecycle_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );
        set_metadata_custom_status(
            &mut terminal,
            "user:claude-title",
            Some("claude"),
            Some("herdr:claude"),
            Some("refactor auth"),
            false,
        );

        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn clearing_user_agent_metadata_restores_hook_custom_status() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_custom_status(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
        );
        set_metadata_custom_status(
            &mut terminal,
            "user:claude-title",
            Some("claude"),
            Some("herdr:claude"),
            Some("refactor auth"),
            false,
        );

        set_metadata_custom_status(&mut terminal, "user:claude-title", None, None, None, true);

        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("thinking")
        );
    }

    #[test]
    fn user_agent_metadata_overrides_presentation_fields_only() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_custom_status(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
        );

        let mutation = terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:presentation".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Refactor auth".into()),
            display_agent: Some("Claude: auth".into()),
            custom_status: Some("middleware".into()),
            state_labels: HashMap::from([("working".into(), "deep in the mines".into())]),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });

        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.effective_agent_label(), Some("claude"));
        let presentation = terminal.effective_presentation();
        assert_eq!(presentation.title.as_deref(), Some("Refactor auth"));
        assert_eq!(presentation.display_agent.as_deref(), Some("Claude: auth"));
        assert_eq!(presentation.custom_status.as_deref(), Some("middleware"));
        assert_eq!(
            presentation.state_labels.get("working").map(String::as_str),
            Some("deep in the mines")
        );
        assert!(mutation.unwrap().effective_state_change.is_some());
    }

    #[test]
    fn metadata_title_takes_pane_border_priority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_manual_label("manual".into());
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:presentation".into(),
            agent_label: Some("claude".into()),
            applies_to_source: None,
            title: Some("Prompt title".into()),
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });

        assert_eq!(
            terminal.border_label(false).as_deref(),
            Some("Prompt title")
        );
        assert_eq!(terminal.border_label(true).as_deref(), Some("Prompt title"));
    }

    #[test]
    fn metadata_without_sequence_can_update_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );

        set_metadata_custom_status(
            &mut terminal,
            "user:claude-title",
            Some("claude"),
            Some("herdr:claude"),
            Some("first"),
            false,
        );
        set_metadata_custom_status(
            &mut terminal,
            "user:claude-title",
            Some("claude"),
            Some("herdr:claude"),
            Some("second"),
            false,
        );

        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("second")
        );
    }

    #[test]
    fn metadata_resolves_newest_value_per_presentation_field() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:title".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Prompt title".into()),
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: Some(1),
        });
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("activity".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: Some(1),
        });

        let presentation = terminal.effective_presentation();
        assert_eq!(presentation.title.as_deref(), Some("Prompt title"));
        assert_eq!(presentation.custom_status.as_deref(), Some("activity"));
    }

    #[test]
    fn partial_update_does_not_refresh_unchanged_field_precedence() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:first".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: Some("First display".into()),
            custom_status: Some("old".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: Some(1),
        });
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:second".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("new".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: Some(1),
        });
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:first".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Fresh title".into()),
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: true,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: Some(2),
        });

        let presentation = terminal.effective_presentation();
        assert_eq!(presentation.title.as_deref(), Some("Fresh title"));
        assert_eq!(presentation.display_agent, None);
        assert_eq!(presentation.custom_status.as_deref(), Some("new"));
    }

    #[test]
    fn metadata_can_set_other_fields_while_clearing_missing_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("activity".into()),
            state_labels: HashMap::new(),
            clear_title: true,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });

        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("activity")
        );
    }

    #[test]
    fn metadata_clear_plus_set_without_ttl_does_not_keep_old_ttl() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Old title".into()),
            display_agent: None,
            custom_status: Some("old".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(1)),
            seq: None,
        });
        let old_deadline = terminal.next_agent_metadata_expiry().unwrap();

        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("fresh".into()),
            state_labels: HashMap::new(),
            clear_title: true,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });

        assert_eq!(terminal.next_agent_metadata_expiry(), None);
        assert_eq!(terminal.effective_custom_status().as_deref(), Some("fresh"));
        assert!(terminal
            .expire_agent_metadata_at(
                old_deadline + Duration::from_millis(1),
                old_deadline + Duration::from_millis(1)
            )
            .is_none());
        assert_eq!(terminal.effective_custom_status().as_deref(), Some("fresh"));
    }

    #[test]
    fn metadata_clear_only_without_ttl_does_not_extend_old_ttl() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Prompt title".into()),
            display_agent: None,
            custom_status: Some("old".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(1)),
            seq: None,
        });
        let old_deadline = terminal.next_agent_metadata_expiry().unwrap();

        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: None,
            applies_to_source: None,
            title: None,
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: true,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });

        assert_eq!(terminal.next_agent_metadata_expiry(), Some(old_deadline));
        assert_eq!(
            terminal.effective_presentation().title.as_deref(),
            Some("Prompt title")
        );
        assert_eq!(terminal.effective_custom_status(), None);

        let mutation = terminal
            .expire_agent_metadata_at(old_deadline, old_deadline)
            .unwrap();
        assert!(mutation.effective_state_change.is_some());
        assert_eq!(terminal.effective_presentation().title, None);
    }

    #[test]
    fn metadata_ttl_expiry_reports_presentation_change() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("activity".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(1)),
            seq: None,
        });

        let deadline = terminal.next_agent_metadata_expiry().unwrap();
        let mutation = terminal
            .expire_agent_metadata_at(deadline, deadline)
            .unwrap();
        let change = mutation.effective_state_change.unwrap();

        assert_eq!(change.previous_state, AgentState::Working);
        assert_eq!(change.state, AgentState::Working);
        assert_eq!(
            change.previous_presentation.custom_status.as_deref(),
            Some("activity")
        );
        assert_eq!(change.presentation.custom_status, None);
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn stale_guarded_metadata_expiry_does_not_report_visible_change() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("stale".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(1)),
            seq: None,
        });
        let deadline = terminal
            .agent_metadata
            .get("user:status")
            .and_then(|metadata| terminal.agent_metadata_expiry(metadata))
            .unwrap();
        assert_eq!(terminal.next_agent_metadata_expiry(), None);
        assert_eq!(terminal.effective_custom_status(), None);

        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        let mutation = terminal.expire_agent_metadata_at(
            deadline + Duration::from_millis(1),
            deadline + Duration::from_millis(1),
        );

        assert!(mutation.is_none());
        assert!(!terminal.agent_metadata.contains_key("user:status"));
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn late_metadata_expiry_reports_all_due_visible_changes() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:first".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("First".into()),
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(1)),
            seq: None,
        });
        let first_deadline = terminal.next_agent_metadata_expiry().unwrap();
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:second".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: Some("Second".into()),
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::from_millis(2)),
            seq: None,
        });
        let second_deadline = terminal
            .agent_metadata
            .get("user:second")
            .and_then(|metadata| terminal.agent_metadata_expiry(metadata))
            .unwrap();

        let mutation = terminal
            .expire_agent_metadata_at(first_deadline, second_deadline)
            .unwrap();
        let change = mutation.effective_state_change.unwrap();

        assert_eq!(change.previous_presentation.title.as_deref(), Some("First"));
        assert_eq!(
            change.previous_presentation.display_agent.as_deref(),
            Some("Second")
        );
        assert_eq!(change.presentation, EffectivePresentation::empty());
        assert!(terminal.agent_metadata.is_empty());
    }

    #[test]
    fn immediately_expired_visible_metadata_still_schedules_expiry_event() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("instant".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::ZERO),
            seq: None,
        });

        let deadline = terminal
            .next_agent_metadata_expiry()
            .expect("pending expiry should be scheduled");
        let mutation = terminal
            .expire_agent_metadata_at(deadline, Instant::now())
            .unwrap();
        let change = mutation.effective_state_change.unwrap();

        assert_eq!(
            change.previous_presentation.custom_status.as_deref(),
            Some("instant")
        );
        assert_eq!(change.presentation.custom_status, None);
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn pending_metadata_expiry_clears_when_lifecycle_guard_hides_metadata() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: None,
            custom_status: Some("instant".into()),
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::ZERO),
            seq: None,
        });
        assert!(terminal.next_agent_metadata_expiry().is_some());

        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.next_agent_metadata_expiry(), None);
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn partial_update_does_not_resurrect_expired_hidden_metadata_fields() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );
        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: Some("Expired title".into()),
            display_agent: None,
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: false,
            clear_state_labels: false,
            ttl: Some(Duration::ZERO),
            seq: None,
        });
        assert_eq!(terminal.next_agent_metadata_expiry(), None);

        terminal.set_agent_metadata(AgentMetadataReport {
            source: "user:status".into(),
            agent_label: Some("claude".into()),
            applies_to_source: Some("herdr:claude".into()),
            title: None,
            display_agent: Some("Fresh display".into()),
            custom_status: None,
            state_labels: HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_custom_status: true,
            clear_state_labels: false,
            ttl: None,
            seq: None,
        });
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
        );

        let presentation = terminal.effective_presentation();
        assert_eq!(presentation.title, None);
        assert_eq!(presentation.display_agent.as_deref(), Some("Fresh display"));
    }
}
