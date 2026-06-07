use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// Effective state arbitration is intentionally centralized here. Hooks are the
// default authority for agent-owned internal state, but a narrow set of strong
// visible screen signals can veto stale hook reports. Precedence is:
// strong visible blocker > visible working/idle recovery > hook > fallback.
// Process-exit updates clear matching hook authority before recomputing state.

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{AgentMetadata, AgentMetadataReport, EffectivePresentation};

const CLAUDE_WORKING_HOLD: Duration = Duration::from_millis(1200);
const STALE_HOOK_IDLE_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub custom_status: Option<String>,
    pub reported_at: Instant,
    pub session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_presentation: EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub presentation: EffectivePresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalStateMutation {
    pub effective_state_change: Option<EffectiveStateChange>,
    pub session_ref_changed: bool,
    /// Session ref a report just attached to this terminal. A session id is
    /// unique to one live agent process, so the caller evicts the same ref
    /// from every other terminal (mis-deliveries from stale pane-id envs).
    pub applied_session_ref: Option<crate::agent_resume::AgentSessionRef>,
}

/// Pure state for a server-owned terminal.
///
/// During the migration this is still one-to-one with a pane-backed PTY, but
/// pane/view state no longer owns terminal identity, cwd, labels, or agent
/// metadata.
pub struct TerminalState {
    pub id: TerminalId,
    pub cwd: PathBuf,
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    fallback_visible_blocker: bool,
    fallback_visible_idle: bool,
    fallback_visible_working: bool,
    fallback_observed_at: Option<Instant>,
    /// The last user prompt submitted to this pane's agent, reported by the
    /// integration hook (Claude's UserPromptSubmit). Shown in the pane header.
    pub last_prompt: Option<String>,
    /// Latched once an agent is ever seen in this pane: keeps the pane header
    /// reservation stable so the PTY doesn't resize on detection flaps.
    pub header_reserved: bool,
    /// When the effective agent state last transitioned. Drives the
    /// oldest-first ordering of the attention queue.
    pub state_changed_at: Option<Instant>,
    /// Free-text activity from the agent's own status line while Working
    /// (e.g. Claude's "Implementing the parser"). Cleared by the detector
    /// when the agent stops working.
    pub live_activity: Option<String>,
    stale_hook_idle_since: Option<Instant>,
    pub hook_authority: Option<HookAuthority>,
    pub agent_metadata: HashMap<String, AgentMetadata>,
    pub persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    hook_report_sequences: HashMap<String, u64>,
    metadata_report_sequences: HashMap<String, u64>,
    pub state: AgentState,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    pub respawn_shell_on_exit: bool,
    pub pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
}

impl TerminalState {
    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            fallback_visible_blocker: false,
            fallback_visible_idle: false,
            fallback_visible_working: false,
            fallback_observed_at: None,
            last_prompt: None,
            header_reserved: false,
            state_changed_at: None,
            live_activity: None,
            stale_hook_idle_since: None,
            hook_authority: None,
            agent_metadata: HashMap::new(),
            persisted_agent_session: None,
            manual_label: None,
            agent_name: None,
            hook_report_sequences: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            state: AgentState::Unknown,
            revision: 0,
            launch_argv: None,
            respawn_shell_on_exit: false,
            pending_agent_resume_plan: None,
        }
    }

    pub fn with_launch_argv(mut self, argv: Vec<String>) -> Self {
        self.launch_argv = Some(argv);
        self
    }

    pub fn with_respawn_shell_on_exit(mut self) -> Self {
        self.respawn_shell_on_exit = true;
        self
    }

    pub fn with_pending_agent_resume_plan(
        mut self,
        plan: crate::agent_resume::AgentResumePlan,
    ) -> Self {
        self.pending_agent_resume_plan = Some(plan);
        self
    }

    #[cfg(test)]
    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_visible_blocker(agent, fallback_state, false, false, false)
    }

    #[cfg(test)]
    pub fn set_detected_state_with_mutation(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> TerminalStateMutation {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            false,
            false,
            false,
            false,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_detected_state_with_visible_blocker(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        visible_idle: bool,
        process_exited: bool,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            visible_blocker,
            visible_idle,
            false,
            process_exited,
            Instant::now(),
        )
        .effective_state_change
    }

    pub fn set_detected_state_with_screen_signals_at(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        visible_idle: bool,
        visible_working: bool,
        process_exited: bool,
        now: Instant,
    ) -> TerminalStateMutation {
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_detected_agent = self.detected_agent;
        let previous_session = self.current_session_identity_for_persistence();
        if agent.is_some() {
            self.header_reserved = true;
        }
        self.detected_agent = agent;
        self.fallback_state = fallback_state;
        self.fallback_visible_blocker = visible_blocker && fallback_state == AgentState::Blocked;
        self.fallback_visible_idle = visible_idle && fallback_state == AgentState::Idle;
        self.fallback_visible_working = visible_working && fallback_state == AgentState::Working;
        self.fallback_observed_at = Some(now);
        if process_exited
            && self.hook_authority_not_newer_than(now)
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::parse_agent_label(&authority.agent_label) == agent
            })
        {
            self.hook_authority = None;
            self.stale_hook_idle_since = None;
        }
        if self.hook_authority_not_newer_than(now)
            && (self.hook_authority_conflicts_with_detected_agent(agent)
                || (previous_detected_agent.is_some()
                    && agent != previous_detected_agent
                    && self.hook_authority.as_ref().is_some_and(|authority| {
                        crate::detect::parse_agent_label(&authority.agent_label)
                            == previous_detected_agent
                    })))
        {
            self.hook_authority = None;
            self.stale_hook_idle_since = None;
        }
        let detected_agent_changed_or_disappeared =
            previous_detected_agent.is_some() && agent != previous_detected_agent;
        let persisted_agent_was_previously_detected =
            self.persisted_agent_session_belongs_to_detected_agent(previous_detected_agent);
        if self.persisted_agent_session_conflicts_with_detected_agent(agent)
            || detected_agent_changed_or_disappeared && persisted_agent_was_previously_detected
        {
            self.persisted_agent_session = None;
        }
        self.update_stale_hook_idle_window(now);
        TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session
                != self.current_session_identity_for_persistence(),
            applied_session_ref: None,
        }
    }

    #[cfg(test)]
    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_with_custom_status(source, agent_label, state, message, None, seq)
    }

    #[cfg(test)]
    pub fn set_hook_authority_with_custom_status(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_with_custom_status_at(
            source,
            agent_label,
            state,
            message,
            custom_status,
            None,
            seq,
            Instant::now(),
        )
        .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn set_hook_authority_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_with_custom_status_at(
            source,
            agent_label,
            state,
            message,
            custom_status,
            session_ref,
            seq,
            Instant::now(),
        )
    }

    pub fn set_hook_authority_with_custom_status_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if !self.accept_hook_report(&source, seq) {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        self.persisted_agent_session = None;
        self.header_reserved = true;
        let applied_session_ref = session_ref.clone();
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            custom_status,
            reported_at: now,
            session_ref,
        });
        self.stale_hook_idle_since = None;
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
            applied_session_ref,
        })
    }

    fn hook_authority_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at <= observed_at)
    }

    fn fallback_not_older_than_hook(&self) -> bool {
        self.hook_authority.as_ref().is_none_or(|authority| {
            self.fallback_observed_at
                .is_some_and(|observed_at| authority.reported_at <= observed_at)
        })
    }

    fn hook_authority_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn persisted_agent_session_conflicts_with_detected_agent(
        &self,
        detected_agent: Option<Agent>,
    ) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.persisted_agent_session
            .as_ref()
            .and_then(|session| crate::detect::parse_agent_label(&session.agent))
            .is_some_and(|agent| agent != detected_agent)
    }

    fn persisted_agent_session_belongs_to_detected_agent(
        &self,
        detected_agent: Option<Agent>,
    ) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.persisted_agent_session
            .as_ref()
            .and_then(|session| crate::detect::parse_agent_label(&session.agent))
            .is_some_and(|agent| agent == detected_agent)
    }

    fn persisted_agent_session_matches(&self, source: &str, agent: &str) -> bool {
        self.persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source == source && session.agent == agent)
    }

    fn current_session_identity_for_persistence(
        &self,
    ) -> Option<(
        String,
        String,
        crate::agent_resume::AgentSessionRefKind,
        String,
    )> {
        if let Some(authority) = self.hook_authority.as_ref() {
            if let Some(session_ref) = authority.session_ref.as_ref() {
                return Some((
                    authority.source.clone(),
                    authority.agent_label.clone(),
                    session_ref.kind,
                    session_ref.value.clone(),
                ));
            }
        }
        self.persisted_agent_session.as_ref().map(|session| {
            (
                session.source.clone(),
                session.agent.clone(),
                session.session_ref.kind,
                session.session_ref.value.clone(),
            )
        })
    }

    pub fn set_persisted_agent_session(
        &mut self,
        session: crate::agent_resume::PersistedAgentSession,
    ) {
        self.persisted_agent_session = Some(session);
    }

    pub fn set_agent_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.header_reserved = true;
        let session_ref = session_ref?;
        if !self.accept_hook_report(&source, seq) {
            return None;
        }
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }

        let previous_session = self.current_session_identity_for_persistence();
        let applied_session_ref = session_ref.clone();
        self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
            source,
            agent: agent_label,
            session_ref,
        });
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: None,
            session_ref_changed: previous_session != current_session,
            applied_session_ref: Some(applied_session_ref),
        })
    }

    /// Drop any agent session identity equal to `session_ref`. A session id
    /// belongs to exactly one live agent process; when an ancestry-verified
    /// report lands that ref on another terminal, every other holder was a
    /// mis-delivery from an environment carrying a stale pane id.
    pub fn evict_session_ref(
        &mut self,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> bool {
        let mut changed = false;
        if let Some(authority) = self.hook_authority.as_mut() {
            if authority.session_ref.as_ref() == Some(session_ref) {
                authority.session_ref = None;
                changed = true;
            }
        }
        if self
            .persisted_agent_session
            .as_ref()
            .is_some_and(|session| &session.session_ref == session_ref)
        {
            self.persisted_agent_session = None;
            changed = true;
        }
        changed
    }

    fn known_agent_label_conflicts_with_detected_agent(&self, agent_label: &str) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label)
            .is_some_and(|hook_agent| hook_agent != detected_agent)
    }

    fn accept_hook_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        let Some(seq) = seq else {
            return !self.hook_report_sequences.contains_key(source);
        };

        if self
            .hook_report_sequences
            .get(source)
            .is_some_and(|last_seq| seq <= *last_seq)
        {
            return false;
        }

        self.hook_report_sequences.insert(source.to_string(), seq);
        true
    }

    #[cfg(test)]
    pub fn clear_hook_authority(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.clear_hook_authority_with_mutation(source, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn clear_hook_authority_with_mutation(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        let sequence_source = source.map(str::to_string).or_else(|| {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.source.clone())
        });
        if let Some(source) = sequence_source.as_deref() {
            if !self.accept_hook_report(source, seq) {
                return None;
            }
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        let should_clear = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        if !should_clear {
            return None;
        }
        self.hook_authority = None;
        self.stale_hook_idle_since = None;
        self.persisted_agent_session = None;
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session.is_some(),
            applied_session_ref: None,
        })
    }

    #[cfg(test)]
    pub fn release_agent(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.release_agent_with_mutation(source, agent_label, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn release_agent_with_mutation(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        if !self.accept_hook_report(source, seq) {
            return None;
        }

        if self.hook_authority.as_ref().is_some_and(|authority| {
            authority.agent_label != agent_label || authority.source != source
        }) {
            return None;
        }

        let matches_current_agent = self.effective_agent_label() == Some(agent_label);
        let matches_persisted_session = self.persisted_agent_session_matches(source, agent_label);
        if !matches_current_agent && !matches_persisted_session {
            return None;
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_visible_idle = false;
        self.fallback_visible_working = false;
        self.fallback_observed_at = None;
        self.hook_authority = None;
        self.stale_hook_idle_since = None;
        self.persisted_agent_session = None;
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session.is_some(),
            applied_session_ref: None,
        })
    }

    pub fn effective_agent_label(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .map(|authority| authority.agent_label.as_str())
            .or_else(|| self.detected_agent.map(crate::detect::agent_label))
    }

    pub fn effective_known_agent(&self) -> Option<Agent> {
        if let Some(authority) = &self.hook_authority {
            return crate::detect::parse_agent_label(&authority.agent_label);
        }
        self.detected_agent
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        self.fallback_visible_blocker
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn visible_working_overrides_hook(&self) -> bool {
        self.fallback_visible_working
            && self.visible_working_is_fresh_enough_for_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                (authority.state == AgentState::Idle || authority.state == AgentState::Blocked)
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn visible_working_is_fresh_enough_for_hook(&self) -> bool {
        self.fallback_not_older_than_hook()
            || self
                .fallback_observed_at
                .zip(
                    self.hook_authority
                        .as_ref()
                        .map(|authority| authority.reported_at),
                )
                .is_some_and(|(observed_at, reported_at)| {
                    reported_at >= observed_at
                        && reported_at.duration_since(observed_at) < CLAUDE_WORKING_HOLD
                })
    }

    fn visible_idle_stales_hook(&self, now: Instant) -> bool {
        self.stale_hook_idle_since
            .is_some_and(|since| now.duration_since(since) >= STALE_HOOK_IDLE_GRACE)
    }

    fn visible_idle_masks_hook_custom_status(&self, state: AgentState, now: Instant) -> bool {
        self.fallback_visible_idle
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                (authority.state == AgentState::Working || authority.state == AgentState::Blocked)
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
            && (state == AgentState::Idle || self.visible_idle_stales_hook(now))
    }

    fn update_stale_hook_idle_window(&mut self, now: Instant) {
        let visible_idle_stales_hook = self.fallback_visible_idle
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                (authority.state == AgentState::Working || authority.state == AgentState::Blocked)
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            });

        if visible_idle_stales_hook {
            self.stale_hook_idle_since.get_or_insert(now);
        } else {
            self.stale_hook_idle_since = None;
        }
    }

    pub fn set_manual_label(&mut self, label: String) {
        let label = label.trim().to_string();
        self.manual_label = (!label.is_empty()).then_some(label);
    }

    pub fn clear_manual_label(&mut self) {
        self.manual_label = None;
    }

    pub fn set_agent_name(&mut self, name: String) {
        let name = name.trim().to_string();
        self.agent_name = (!name.is_empty()).then_some(name);
    }

    pub fn clear_agent_name(&mut self) {
        self.agent_name = None;
    }

    pub fn clear_agent_runtime_identity_after_respawn(&mut self) {
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_visible_idle = false;
        self.fallback_visible_working = false;
        self.fallback_observed_at = None;
        self.stale_hook_idle_since = None;
        self.hook_authority = None;
        self.persisted_agent_session = None;
        self.agent_metadata.clear();
        self.state = AgentState::Unknown;
        self.launch_argv = None;
        self.respawn_shell_on_exit = false;
        self.pending_agent_resume_plan = None;
        self.clear_agent_name();
    }

    pub fn is_agent_terminal(&self) -> bool {
        self.agent_name.is_some()
            || self.effective_agent_label().is_some()
            || self.launch_argv.is_some()
    }

    pub fn border_label(&self, show_agent_labels: bool) -> Option<String> {
        self.effective_title().or_else(|| {
            self.manual_label.clone().or_else(|| {
                show_agent_labels
                    .then(|| {
                        self.effective_display_agent()
                            .or_else(|| self.effective_agent_label().map(str::to_string))
                    })
                    .flatten()
            })
        })
    }

    fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
        previous_presentation: EffectivePresentation,
        now: Instant,
    ) -> Option<EffectiveStateChange> {
        let state = if self.visible_blocker_overrides_hook() {
            AgentState::Blocked
        } else if self.visible_working_overrides_hook() {
            AgentState::Working
        } else if self.visible_idle_stales_hook(now) {
            AgentState::Idle
        } else {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.state)
                .unwrap_or(self.fallback_state)
        };
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();

        let presentation = self.effective_presentation_for_state_at(state, now);
        self.clear_expiry_pending_for_hidden_metadata();

        if previous_agent_label == agent_label
            && previous_state == state
            && previous_presentation == presentation
        {
            return None;
        }

        if previous_state != state {
            self.state_changed_at = Some(now);
        }
        self.state = state;
        Some(EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            agent_label,
            known_agent,
            state,
            presentation,
        })
    }
}

pub(crate) fn stabilize_agent_state(
    agent: Option<Agent>,
    previous: AgentState,
    raw: AgentState,
    now: std::time::Instant,
    last_claude_working_at: &mut Option<std::time::Instant>,
) -> AgentState {
    if agent != Some(Agent::Claude) {
        return raw;
    }

    match raw {
        AgentState::Working => {
            *last_claude_working_at = Some(now);
            AgentState::Working
        }
        AgentState::Blocked => AgentState::Blocked,
        AgentState::Idle if previous == AgentState::Working => {
            if last_claude_working_at
                .is_some_and(|last_working| now.duration_since(last_working) < CLAUDE_WORKING_HOLD)
            {
                AgentState::Working
            } else {
                AgentState::Idle
            }
        }
        _ => raw,
    }
}

pub(crate) fn stabilize_agent_detection(
    agent: Option<Agent>,
    previous: AgentState,
    detection: crate::detect::AgentDetection,
    process_exited: bool,
    now: std::time::Instant,
    last_claude_working_at: &mut Option<std::time::Instant>,
) -> AgentState {
    if process_exited {
        return detection.state;
    }

    stabilize_agent_state(
        agent,
        previous,
        detection.state,
        now,
        last_claude_working_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentDetection;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    #[test]
    fn claude_working_is_sticky_for_short_gap() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Idle,
            AgentState::Working,
            now,
            &mut last_working,
        );
        assert_eq!(working, AgentState::Working);

        let still_working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + std::time::Duration::from_millis(400),
            &mut last_working,
        );
        assert_eq!(still_working, AgentState::Working);
    }

    #[test]
    fn claude_transitions_to_idle_after_hold_expires() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + CLAUDE_WORKING_HOLD + std::time::Duration::from_millis(1),
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn process_exit_idle_bypasses_claude_working_hold() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_detection(
            Some(Agent::Claude),
            AgentState::Working,
            AgentDetection {
                state: AgentState::Idle,
                activity: None,
                skip_state_update: false,
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
            },
            true,
            now + std::time::Duration::from_millis(100),
            &mut last_working,
        );

        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn visible_idle_does_not_bypass_claude_working_hold() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_detection(
            Some(Agent::Claude),
            AgentState::Working,
            AgentDetection {
                state: AgentState::Idle,
                activity: None,
                skip_state_update: false,
                visible_blocker: false,
                visible_idle: true,
                visible_working: false,
            },
            false,
            now + std::time::Duration::from_millis(100),
            &mut last_working,
        );

        assert_eq!(state, AgentState::Working);
    }

    #[test]
    fn non_claude_states_are_unchanged() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let state = stabilize_agent_state(
            Some(Agent::Codex),
            AgentState::Working,
            AgentState::Idle,
            now,
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_can_override_with_unknown_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_overrides_non_blocked_hook_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(change.unwrap().previous_state, AgentState::Working);
    }

    #[test]
    fn weak_blocked_fallback_does_not_override_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            false,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn hook_blocked_wins_over_visible_blocker() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn visible_blocker_does_not_override_different_agent_hook() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(None, AgentState::Unknown);
        terminal.set_hook_authority(
            "custom:agent".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_suppresses_stale_hook_custom_status() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            Some("planning".into()),
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn visible_idle_waits_before_overriding_claude_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
            None,
            now,
        );

        let waiting = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(500),
        );

        assert!(waiting.effective_state_change.is_none());
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("thinking")
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(500) + STALE_HOOK_IDLE_GRACE + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.effective_custom_status(), None);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn fresh_hook_working_resets_visible_idle_stale_window() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
            None,
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(500),
        );

        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
            Some(1),
            now + Duration::from_millis(800),
        );
        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + STALE_HOOK_IDLE_GRACE + Duration::from_millis(1),
        );

        assert!(change.effective_state_change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_working_overrides_hook_idle_for_same_agent() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Idle
        );
    }

    #[test]
    fn recent_visible_working_holds_against_newer_claude_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );

        let change = terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now + Duration::from_millis(100),
        );

        assert!(change.unwrap().effective_state_change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn old_visible_working_does_not_hold_against_newer_claude_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );

        let change = terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now + CLAUDE_WORKING_HOLD + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change
                .unwrap()
                .effective_state_change
                .unwrap()
                .previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn refreshed_visible_working_overrides_newer_hook_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some("permission".into()),
            None,
            None,
            now + CLAUDE_WORKING_HOLD + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Blocked);

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + CLAUDE_WORKING_HOLD + Duration::from_millis(800),
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.effective_custom_status(), None);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Blocked
        );
    }

    #[test]
    fn visible_idle_waits_before_overriding_claude_hook_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            Some("permission".into()),
            None,
            None,
            now,
        );

        let waiting = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(500),
        );

        assert!(waiting.effective_state_change.is_none());
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("permission")
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_millis(500) + STALE_HOOK_IDLE_GRACE + Duration::from_millis(1),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(terminal.effective_custom_status(), None);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Blocked
        );
    }

    #[test]
    fn visible_idle_does_not_override_other_agent_hook_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn known_hook_authority_does_not_override_different_detected_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);
        let change = terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        assert!(change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clears_conflicting_known_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn border_label_prefers_manual_label_over_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        assert_eq!(terminal.border_label(false), None);
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label(" reviewer ".into());
        assert_eq!(terminal.border_label(false).as_deref(), Some("reviewer"));
        assert_eq!(terminal.border_label(true).as_deref(), Some("reviewer"));

        terminal.set_manual_label("   ".into());
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label("reviewer".into());
        terminal.clear_manual_label();
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));
    }

    #[test]
    fn hook_authority_survives_unrelated_detected_agent_clear() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clear_clears_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:opencode".into(),
            "opencode".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn detected_agent_clear_clears_matching_working_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn process_exit_clears_matching_hook_authority_before_reporting_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
        assert_eq!(terminal.effective_agent_label(), Some("codex"));
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn stale_visible_screen_signal_does_not_override_newer_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(1),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            observed,
        );

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.stale_hook_idle_since.is_none());
    }

    #[test]
    fn stale_process_exit_does_not_clear_newer_same_agent_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(1),
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            Some("new turn".into()),
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed,
        );

        let authority = terminal.hook_authority.as_ref().expect("hook authority");
        assert_eq!(authority.custom_status.as_deref(), Some("new turn"));
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.effective_agent_label(), Some("codex"));
    }

    #[test]
    fn detected_agent_change_clears_previous_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::OpenCode));
        assert_eq!(terminal.effective_agent_label(), Some("opencode"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn release_agent_clears_identity_immediately() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.release_agent("herdr:pi", "pi", None);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn stale_hook_report_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn accepted_hook_report_stores_session_ref() {
        let mut terminal = test_terminal();
        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path("/tmp/pi.jsonl"),
                Some(20),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| (&session_ref.kind, session_ref.value.as_str())),
            Some((
                &crate::agent_resume::AgentSessionRefKind::Path,
                "/tmp/pi.jsonl"
            ))
        );
    }

    #[test]
    fn stale_hook_report_cannot_overwrite_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi.jsonl"),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/new.jsonl"),
            Some(19),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some("/tmp/pi.jsonl")
        );
    }

    #[test]
    fn accepted_hook_report_without_session_ref_clears_previous_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi.jsonl"),
            Some(20),
        );

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(21),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal
            .hook_authority
            .as_ref()
            .unwrap()
            .session_ref
            .is_none());
    }

    #[test]
    fn accepted_hook_report_marks_changed_when_session_identity_changes() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:opencode".into(),
            agent: "opencode".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("same-session").unwrap(),
        });

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "herdr:hermes".into(),
                "hermes".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("same-session"),
                Some(20),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
    }

    #[test]
    fn clearing_hook_authority_clears_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi.jsonl"),
            Some(20),
        );

        let mutation = terminal
            .clear_hook_authority_with_mutation(Some("herdr:pi"), Some(21))
            .expect("accepted clear");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn release_agent_clears_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi.jsonl"),
            Some(20),
        );

        let mutation = terminal
            .release_agent_with_mutation("herdr:pi", "pi", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn release_agent_clears_matching_restored_session_ref_before_detection() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal
            .release_agent_with_mutation("herdr:hermes", "hermes", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn respawn_cleanup_resets_restored_agent_status() {
        let mut terminal = test_terminal();
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("codex".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);

        terminal.clear_agent_runtime_identity_after_respawn();

        assert_eq!(terminal.state, AgentState::Unknown);
        assert!(terminal.detected_agent.is_none());
        assert!(terminal.agent_name.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert!(!terminal.respawn_shell_on_exit);
    }

    #[test]
    fn detected_conflict_clears_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(20),
        );

        let mutation =
            terminal.set_detected_state_with_mutation(Some(Agent::Grok), AgentState::Idle);

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn detected_agent_disappearance_clears_matching_hook_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "herdr:hermes".into(),
            "hermes".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-session"),
            Some(20),
        );

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(terminal.effective_agent_label(), None);
    }

    #[test]
    fn detected_agent_disappearance_clears_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:opencode".into(),
            agent: "opencode".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        });

        let first =
            terminal.set_detected_state_with_mutation(Some(Agent::OpenCode), AgentState::Idle);
        assert!(!first.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());

        let second = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(second.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn initial_unknown_detection_preserves_restored_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn unsequenced_hook_report_is_ignored_after_source_uses_sequence() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_release_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.release_agent("herdr:pi", "pi", Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn stale_clear_all_sequence_is_checked_against_current_authority_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.clear_hook_authority(None, Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn same_sequence_from_different_sources_is_independent() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "herdr:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.set_hook_authority(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().source,
            "custom:pi"
        );
    }
}
