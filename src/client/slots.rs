//! Connection slots (#65) — the multi-connection client core.
//!
//! A *slot* is one framed server connection the client holds open: the home
//! (local) unix socket, or a fleet peer reached over the existing ssh-stdio
//! bridge (which presents the remote server as a local forwarded socket). The
//! client owns the terminal forever; switching servers flips which slot feeds
//! the painter and receives input, instead of exiting and relaunching an
//! attach leg.
//!
//! Policy is *warm-all*: at start the client background-dials every configured
//! fleet target so a later switch to any of them is an instant in-process flip
//! (resume frames + focus) rather than an ssh dial. Home is always warm. Failed
//! dials fall back to cold with gentle backoff; the exit-and-relaunch legs in
//! `main.rs` remain as the cold-dial / ssh-bootstrap path.
//!
//! Two layers live here. [`SlotRegistry`] is the pure flip / pause / resume /
//! demote / backoff state machine plus the warm-all target derivation — no
//! I/O, so it is exercised entirely in unit tests. [`SlotManager`] wraps it
//! with the live warm/active [`SlotConnection`]s and turns registry effects
//! into `SetFrameSubscription` wire messages; the reader threads and the
//! active-stream swap live in the client event loop (`client/mod.rs`).

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::protocol::{ClientMessage, HOME_SWITCH_TARGET};

/// Backoff applied to a cold slot whose last dial failed, before it is
/// eligible to be re-dialed. Gentle — a down server should ghost, not spin.
const COLD_REDIAL_BACKOFF: Duration = Duration::from_secs(15);

/// A slot's dial target. `Home` is the local server (always warm); `Ssh`
/// names a fleet peer's ssh destination (the same string a `SwitchServer`
/// carries and the launcher would dial).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SlotTarget {
    /// The local server — home. Reached via the local client socket.
    Home,
    /// A fleet peer reached over the ssh-stdio bridge by this ssh destination.
    Ssh(String),
}

impl SlotTarget {
    /// The switch-target string for this slot: the reserved home sentinel for
    /// home, or the ssh destination for a peer. This is exactly the key a
    /// `SwitchServer { ssh_target }` resolves against.
    pub(crate) fn key(&self) -> &str {
        match self {
            SlotTarget::Home => HOME_SWITCH_TARGET,
            SlotTarget::Ssh(target) => target.as_str(),
        }
    }

    /// Build a slot target from a switch-target string (the inverse of
    /// [`key`]): the reserved sentinel maps to home, anything else to an ssh
    /// peer.
    pub(crate) fn from_key(key: &str) -> Self {
        if key == HOME_SWITCH_TARGET {
            SlotTarget::Home
        } else {
            SlotTarget::Ssh(key.to_string())
        }
    }
}

/// Lifecycle phase of one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotPhase {
    /// The active slot: frames painted, input forwarded. At most one.
    Active,
    /// Connection held, frames paused. An instant flip away.
    Warm,
    /// No connection. Lazy-dialed on first switch (or by the warm-all dialer).
    /// `failed_at` records the last failed dial so backoff can gate redials.
    Cold { failed_at: Option<Instant> },
}

/// One registry entry.
#[derive(Debug)]
struct Slot {
    target: SlotTarget,
    phase: SlotPhase,
}

/// What the caller must do after a registry mutation, so the registry stays
/// pure (no I/O) and the effects are unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlotEffect {
    /// Pause frames on this slot (it stopped being active): send
    /// `SetFrameSubscription { enabled: false }`.
    Pause(SlotTarget),
    /// Resume frames + full redraw on this slot (it became active): send
    /// `SetFrameSubscription { enabled: true }`.
    Resume(SlotTarget),
    /// Background-dial this cold target to warm it.
    Dial(SlotTarget),
}

/// Outcome of a switch request resolved against the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SwitchOutcome {
    /// The target was warm: it is now active. Effects pause the old active slot
    /// and resume the new one — an instant in-process flip, no process exit.
    Flipped { effects: Vec<SlotEffect> },
    /// The target is cold or unknown: fall back to the dial / relaunch-leg
    /// path (the #67 frozen-frame UX) to establish it.
    ColdDial(SlotTarget),
    /// The target is already the active slot: nothing to do.
    AlreadyActive,
}

/// The connection-slot registry.
pub(crate) struct SlotRegistry {
    slots: HashMap<String, Slot>,
    /// Key of the currently active slot, if any.
    active: Option<String>,
    /// Sanity cap on warmed slots (`[slots] max`), including home and active.
    max_warm: usize,
}

impl SlotRegistry {
    /// Create a registry over `targets` with `home` already active. Every other
    /// target starts cold; [`pending_dials`] yields them (bounded by the cap)
    /// for the warm-all dialer to warm in the background.
    pub(crate) fn new(active: SlotTarget, targets: Vec<SlotTarget>, max_warm: usize) -> Self {
        let mut slots = HashMap::new();
        let active_key = active.key().to_string();
        slots.insert(
            active_key.clone(),
            Slot {
                target: active,
                phase: SlotPhase::Active,
            },
        );
        for target in targets {
            slots
                .entry(target.key().to_string())
                .or_insert_with(|| Slot {
                    target: target.clone(),
                    phase: SlotPhase::Cold { failed_at: None },
                });
        }
        Self {
            slots,
            active: Some(active_key),
            max_warm: max_warm.max(1),
        }
    }

    /// Number of slots currently holding a connection (active + warm).
    fn warm_count(&self) -> usize {
        self.slots
            .values()
            .filter(|s| matches!(s.phase, SlotPhase::Active | SlotPhase::Warm))
            .count()
    }

    /// The active slot's target, if any.
    #[cfg(test)]
    pub(crate) fn active_target(&self) -> Option<&SlotTarget> {
        self.active
            .as_ref()
            .and_then(|k| self.slots.get(k))
            .map(|s| &s.target)
    }

    /// Phase of a target, if it is registered.
    #[cfg(test)]
    pub(crate) fn phase(&self, target: &SlotTarget) -> Option<SlotPhase> {
        self.slots.get(target.key()).map(|s| s.phase)
    }

    /// Cold targets eligible to be warmed now: under the cap, and past their
    /// backoff if a prior dial failed. The warm-all dialer drives these, then
    /// reports each result back via [`mark_warm`] / [`mark_dial_failed`].
    pub(crate) fn pending_dials(&self, now: Instant) -> Vec<SlotEffect> {
        let mut budget = self.max_warm.saturating_sub(self.warm_count());
        let mut effects = Vec::new();
        // Deterministic order: home first, then ssh targets by key, so the
        // dialer and tests see a stable sequence.
        let mut cold: Vec<&Slot> = self
            .slots
            .values()
            .filter(|s| match s.phase {
                SlotPhase::Cold { failed_at } => failed_at
                    .map(|at| now.duration_since(at) >= COLD_REDIAL_BACKOFF)
                    .unwrap_or(true),
                _ => false,
            })
            .collect();
        cold.sort_by(|a, b| dial_order(&a.target).cmp(&dial_order(&b.target)));
        for slot in cold {
            if budget == 0 {
                break;
            }
            effects.push(SlotEffect::Dial(slot.target.clone()));
            budget -= 1;
        }
        effects
    }

    /// A background dial succeeded: the target now holds a paused connection.
    /// Returns the pause effect so the caller sends the slot straight into its
    /// warm (frames-off) state.
    pub(crate) fn mark_warm(&mut self, target: &SlotTarget) -> Option<SlotEffect> {
        let slot = self.slots.get_mut(target.key())?;
        if matches!(slot.phase, SlotPhase::Active) {
            return None;
        }
        slot.phase = SlotPhase::Warm;
        Some(SlotEffect::Pause(target.clone()))
    }

    /// A background dial failed: keep the target cold and stamp it for backoff
    /// so it ghosts as today and is gently re-dialed later.
    pub(crate) fn mark_dial_failed(&mut self, target: &SlotTarget, now: Instant) {
        if let Some(slot) = self.slots.get_mut(target.key()) {
            if !matches!(slot.phase, SlotPhase::Active | SlotPhase::Warm) {
                slot.phase = SlotPhase::Cold {
                    failed_at: Some(now),
                };
            }
        }
    }

    /// Transport death of a warm (or active) slot: demote it to cold silently.
    /// The failure surfaces only if the user later switches to it (#65). A
    /// dead active slot leaves the registry with no active slot — the caller
    /// must reattach it (the only slot driving the terminal).
    pub(crate) fn demote_dead(&mut self, target: &SlotTarget) {
        if let Some(slot) = self.slots.get_mut(target.key()) {
            slot.phase = SlotPhase::Cold { failed_at: None };
            if self.active.as_deref() == Some(target.key()) {
                self.active = None;
            }
        }
    }

    /// Resolve a switch request against the registry. A warm target flips in
    /// process (pause old, resume new). A cold/unknown target falls back to the
    /// dial path. Re-selecting the active slot is a no-op.
    pub(crate) fn request_switch(&mut self, target: &SlotTarget) -> SwitchOutcome {
        let key = target.key().to_string();
        if self.active.as_deref() == Some(key.as_str()) {
            return SwitchOutcome::AlreadyActive;
        }
        match self.slots.get(&key).map(|s| s.phase) {
            Some(SlotPhase::Warm) => {
                let mut effects = Vec::new();
                if let Some(old_key) = self.active.take() {
                    if let Some(old) = self.slots.get_mut(&old_key) {
                        old.phase = SlotPhase::Warm;
                        effects.push(SlotEffect::Pause(old.target.clone()));
                    }
                }
                if let Some(new) = self.slots.get_mut(&key) {
                    new.phase = SlotPhase::Active;
                    effects.push(SlotEffect::Resume(new.target.clone()));
                }
                self.active = Some(key);
                SwitchOutcome::Flipped { effects }
            }
            // Cold, dead, or never-registered: dial it the slow way. Register
            // an unknown target cold so a subsequent warm reattaches it.
            _ => {
                self.slots.entry(key).or_insert_with(|| Slot {
                    target: target.clone(),
                    phase: SlotPhase::Cold { failed_at: None },
                });
                SwitchOutcome::ColdDial(target.clone())
            }
        }
    }
}

/// Stable dial ordering: home first, then ssh targets alphabetically.
fn dial_order(target: &SlotTarget) -> (u8, &str) {
    match target {
        SlotTarget::Home => (0, ""),
        SlotTarget::Ssh(t) => (1, t.as_str()),
    }
}

/// Derive the warm-all target list for a client, deduplicated and bounded by
/// the slots cap. Home is always included and always first. The rest come from
/// the active server's fleet: the carried snapshot's peers and origin (a spoke
/// learns its fleet from the down-gossip, #73) plus the locally configured
/// `[[peers]]` (a hub knows its own fleet). The reserved home sentinel is never
/// re-added as a peer.
pub(crate) fn warm_all_targets(
    config_peers: &[String],
    carried_peer_targets: &[String],
    max: usize,
) -> Vec<SlotTarget> {
    let mut out = vec![SlotTarget::Home];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(HOME_SWITCH_TARGET.to_string());
    for key in config_peers.iter().chain(carried_peer_targets.iter()) {
        if key.is_empty() || key == HOME_SWITCH_TARGET {
            continue;
        }
        if seen.insert(key.clone()) {
            out.push(SlotTarget::Ssh(key.clone()));
        }
        // Cap includes home, so stop once the list reaches `max`.
        if out.len() >= max.max(1) {
            break;
        }
    }
    out
}

/// What the client loop should do with a slot-tagged event at APPLY time,
/// decided by comparing the reader's slot key against the currently-active
/// slot (#65). This is the apply-time check that makes warm-slot death silent
/// and stale frames harmless — a frame queued by the old reader before a flip
/// arrives tagged with the old slot's key and is [`Drop`](SlotRouting::Drop)ped
/// instead of painting over the new slot's redraw (blocker 1 + 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotRouting {
    /// The event is from the active slot: apply it normally.
    Apply,
    /// The event is from a non-active slot and carries no lifecycle meaning
    /// (a frame, notify, etc.): drop it silently.
    Drop,
    /// The event is a non-active slot's transport/lifecycle death (its reader
    /// disconnected, or its server sent ServerShutdown): demote that slot to
    /// cold silently. The active session is untouched.
    DemoteDead,
}

/// Route a slot-tagged event read by `event_slot` against the `active` slot.
/// `is_lifecycle_death` is true for a reader disconnect or a `ServerShutdown`
/// message (the two signals that a non-active slot's transport is gone).
pub(crate) fn route_slot_event(
    event_slot: &str,
    active: &str,
    is_lifecycle_death: bool,
) -> SlotRouting {
    if event_slot == active {
        SlotRouting::Apply
    } else if is_lifecycle_death {
        SlotRouting::DemoteDead
    } else {
        SlotRouting::Drop
    }
}

/// A live warm/active slot connection the client holds open: the writable
/// stream half plus the slot's target. Frames arrive on the shared loop event
/// channel (the reader half lives in a spawned thread); a paused slot's server
/// stops streaming, so the active slot is the only one painting.
pub(crate) struct SlotConnection {
    pub(crate) target: SlotTarget,
    pub(crate) write_stream: UnixStream,
}

impl SlotConnection {
    /// Send the frame-subscription toggle for this slot (pause when it stops
    /// being active, resume + full redraw when it becomes active).
    pub(crate) fn set_frame_subscription(&mut self, enabled: bool) -> std::io::Result<()> {
        crate::protocol::write_message(
            &mut self.write_stream,
            &ClientMessage::SetFrameSubscription { enabled },
        )
        .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

/// Owns the live warm/active slot connections alongside the [`SlotRegistry`]
/// state machine, and turns [`SlotEffect`]s into wire messages. The active
/// slot's write stream is what the client loop forwards input to; a flip swaps
/// it. Held by the client across an in-process server switch, so the terminal
/// is never released.
pub(crate) struct SlotManager {
    pub(crate) registry: SlotRegistry,
    /// Warm connections keyed by slot key. The active slot is also here.
    connections: HashMap<String, SlotConnection>,
}

impl SlotManager {
    pub(crate) fn new(active: SlotConnection, targets: Vec<SlotTarget>, max_warm: usize) -> Self {
        let registry = SlotRegistry::new(active.target.clone(), targets, max_warm);
        let mut connections = HashMap::new();
        connections.insert(active.target.key().to_string(), active);
        Self {
            registry,
            connections,
        }
    }

    /// Register a freshly-dialed warm connection and pause its frames at the
    /// server. The connection joins the registry as warm.
    pub(crate) fn add_warm(&mut self, mut conn: SlotConnection) -> std::io::Result<()> {
        let key = conn.target.key().to_string();
        if let Some(SlotEffect::Pause(_)) = self.registry.mark_warm(&conn.target) {
            conn.set_frame_subscription(false)?;
        }
        self.connections.insert(key, conn);
        Ok(())
    }

    /// Resolve a switch and, when the target is warm, perform the in-process
    /// flip: pause the old active slot, resume the new one (full redraw), and
    /// return the new active slot's write stream so the loop can rebind input.
    /// A cold/unknown target returns `Ok(None)` — the caller falls back to the
    /// dial / relaunch-leg path (#67 frozen frame).
    pub(crate) fn flip_to(&mut self, target: &SlotTarget) -> std::io::Result<Option<UnixStream>> {
        match self.registry.request_switch(target) {
            SwitchOutcome::AlreadyActive => Ok(None),
            SwitchOutcome::ColdDial(_) => Ok(None),
            SwitchOutcome::Flipped { effects } => {
                for effect in &effects {
                    match effect {
                        SlotEffect::Pause(t) => {
                            if let Some(conn) = self.connections.get_mut(t.key()) {
                                // Best-effort: a dead warm slot we are leaving
                                // is harmless, it just stops painting.
                                let _ = conn.set_frame_subscription(false);
                            }
                        }
                        SlotEffect::Resume(t) => {
                            let conn = self.connections.get_mut(t.key()).ok_or_else(|| {
                                std::io::Error::other("warm slot missing connection")
                            })?;
                            conn.set_frame_subscription(true)?;
                        }
                        SlotEffect::Dial(_) => {}
                    }
                }
                let new_stream = self
                    .connections
                    .get(target.key())
                    .map(|c| c.write_stream.try_clone())
                    .transpose()?;
                Ok(new_stream)
            }
        }
    }

    /// Drop a dead slot's connection and demote it in the registry.
    pub(crate) fn handle_dead(&mut self, target: &SlotTarget) {
        self.connections.remove(target.key());
        self.registry.demote_dead(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(t: &str) -> SlotTarget {
        SlotTarget::Ssh(t.to_string())
    }

    #[test]
    fn warm_all_targets_dedup_home_first_and_capped() {
        let targets = warm_all_targets(
            &["anvil".into(), "sage".into()],
            &["sage".into(), "<home>".into(), "mba".into()],
            8,
        );
        assert_eq!(
            targets,
            vec![SlotTarget::Home, ssh("anvil"), ssh("sage"), ssh("mba")]
        );

        // Cap of 2 keeps home + one peer.
        let capped = warm_all_targets(&["anvil".into(), "sage".into()], &[], 2);
        assert_eq!(capped, vec![SlotTarget::Home, ssh("anvil")]);
    }

    #[test]
    fn new_registry_makes_home_active_and_rest_cold() {
        let reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("anvil"), ssh("sage")], 8);
        assert_eq!(reg.active_target(), Some(&SlotTarget::Home));
        assert_eq!(reg.phase(&SlotTarget::Home), Some(SlotPhase::Active));
        assert_eq!(
            reg.phase(&ssh("anvil")),
            Some(SlotPhase::Cold { failed_at: None })
        );
    }

    #[test]
    fn pending_dials_are_capped_and_home_first() {
        let reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("b"), ssh("a")], 2);
        // Cap 2, home is active (warm_count 1), so only one cold dial fits.
        let dials = reg.pending_dials(Instant::now());
        assert_eq!(dials, vec![SlotEffect::Dial(ssh("a"))]);
    }

    #[test]
    fn dial_failure_applies_backoff_then_redials() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("a")], 8);
        let t0 = Instant::now();
        reg.mark_dial_failed(&ssh("a"), t0);
        // Immediately: still cold but inside backoff, not redialed.
        assert_eq!(reg.pending_dials(t0), vec![]);
        // After the backoff window: eligible again.
        let later = t0 + COLD_REDIAL_BACKOFF + Duration::from_secs(1);
        assert_eq!(reg.pending_dials(later), vec![SlotEffect::Dial(ssh("a"))]);
    }

    #[test]
    fn mark_warm_pauses_the_slot() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("a")], 8);
        let effect = reg.mark_warm(&ssh("a"));
        assert_eq!(effect, Some(SlotEffect::Pause(ssh("a"))));
        assert_eq!(reg.phase(&ssh("a")), Some(SlotPhase::Warm));
    }

    #[test]
    fn switch_to_warm_flips_without_dial() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("a")], 8);
        reg.mark_warm(&ssh("a"));
        let outcome = reg.request_switch(&ssh("a"));
        match outcome {
            SwitchOutcome::Flipped { effects } => {
                // Old active (home) paused, new active (a) resumed.
                assert_eq!(
                    effects,
                    vec![
                        SlotEffect::Pause(SlotTarget::Home),
                        SlotEffect::Resume(ssh("a")),
                    ]
                );
            }
            other => panic!("expected Flipped, got {other:?}"),
        }
        assert_eq!(reg.active_target(), Some(&ssh("a")));
        assert_eq!(reg.phase(&SlotTarget::Home), Some(SlotPhase::Warm));
    }

    #[test]
    fn switch_to_cold_falls_back_to_dial() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("a")], 8);
        // a is still cold (never warmed).
        assert_eq!(
            reg.request_switch(&ssh("a")),
            SwitchOutcome::ColdDial(ssh("a"))
        );
        // Active is unchanged — we never left home.
        assert_eq!(reg.active_target(), Some(&SlotTarget::Home));
    }

    #[test]
    fn switch_to_unknown_registers_cold_and_dials() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![], 8);
        assert_eq!(
            reg.request_switch(&ssh("new")),
            SwitchOutcome::ColdDial(ssh("new"))
        );
        assert_eq!(
            reg.phase(&ssh("new")),
            Some(SlotPhase::Cold { failed_at: None })
        );
    }

    #[test]
    fn switch_to_active_is_a_noop() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![], 8);
        assert_eq!(
            reg.request_switch(&SlotTarget::Home),
            SwitchOutcome::AlreadyActive
        );
    }

    #[test]
    fn demote_dead_warm_slot_is_silent_and_redials_later() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![ssh("a")], 8);
        reg.mark_warm(&ssh("a"));
        reg.demote_dead(&ssh("a"));
        assert_eq!(
            reg.phase(&ssh("a")),
            Some(SlotPhase::Cold { failed_at: None })
        );
        // Active (home) is untouched: the death is silent.
        assert_eq!(reg.active_target(), Some(&SlotTarget::Home));
        // A switch to the dead slot now falls back to a fresh dial.
        assert_eq!(
            reg.request_switch(&ssh("a")),
            SwitchOutcome::ColdDial(ssh("a"))
        );
    }

    #[test]
    fn demote_dead_active_slot_clears_active() {
        let mut reg = SlotRegistry::new(SlotTarget::Home, vec![], 8);
        reg.demote_dead(&SlotTarget::Home);
        assert_eq!(reg.active_target(), None);
    }

    // --- SlotManager transport tests (real socketpairs, no server) ---

    fn read_one_client_message(stream: &mut UnixStream) -> ClientMessage {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        crate::protocol::read_message(stream, crate::protocol::MAX_FRAME_SIZE).unwrap()
    }

    /// The load-bearing #65 behavior: switching to a WARM slot flips in process
    /// — the manager swaps the active write stream and toggles subscriptions on
    /// the wire — WITHOUT any relaunch/respawn. The observable here is the
    /// returned new stream (no switch-file path) plus the pause/resume frames
    /// the peers actually receive.
    #[test]
    fn flip_to_warm_slot_swaps_in_process_no_respawn() {
        // Active = home; its peer end is `home_peer`.
        let (home_local, mut home_peer) = UnixStream::pair().unwrap();
        let mut manager = SlotManager::new(
            SlotConnection {
                target: SlotTarget::Home,
                write_stream: home_local,
            },
            vec![ssh("anvil")],
            8,
        );

        // Warm the anvil slot with its own socketpair; add_warm pauses it.
        let (anvil_local, mut anvil_peer) = UnixStream::pair().unwrap();
        manager
            .add_warm(SlotConnection {
                target: ssh("anvil"),
                write_stream: anvil_local,
            })
            .unwrap();
        // anvil received a pause on warm registration.
        assert_eq!(
            read_one_client_message(&mut anvil_peer),
            ClientMessage::SetFrameSubscription { enabled: false }
        );

        // Flip to anvil: returns its stream (in-process swap, no relaunch).
        let new_stream = manager.flip_to(&ssh("anvil")).unwrap();
        assert!(
            new_stream.is_some(),
            "warm flip must return a stream to rebind input, not fall back to dial"
        );
        // home (old active) was paused; anvil (new active) was resumed.
        assert_eq!(
            read_one_client_message(&mut home_peer),
            ClientMessage::SetFrameSubscription { enabled: false }
        );
        assert_eq!(
            read_one_client_message(&mut anvil_peer),
            ClientMessage::SetFrameSubscription { enabled: true }
        );
        assert_eq!(manager.registry.active_target(), Some(&ssh("anvil")));

        // Switching BACK to home must ALSO be an instant flip (home stayed
        // warm), not a respawn — the previous server is still a held slot.
        let back = manager.flip_to(&SlotTarget::Home).unwrap();
        assert!(back.is_some(), "switching back must flip, not respawn");
        assert_eq!(
            read_one_client_message(&mut anvil_peer),
            ClientMessage::SetFrameSubscription { enabled: false }
        );
        assert_eq!(
            read_one_client_message(&mut home_peer),
            ClientMessage::SetFrameSubscription { enabled: true }
        );
        assert_eq!(manager.registry.active_target(), Some(&SlotTarget::Home));
    }

    // --- Apply-time slot routing (#65 blockers 1 + 2) ---

    #[test]
    fn route_active_slot_frame_applies() {
        // A frame tagged with the active slot's key paints normally.
        assert_eq!(
            route_slot_event(HOME_SWITCH_TARGET, HOME_SWITCH_TARGET, false),
            SlotRouting::Apply
        );
    }

    #[test]
    fn route_stale_frame_from_old_slot_is_dropped() {
        // The load-bearing apply-time check: after a flip to "anvil", a frame
        // the OLD home reader had already queued arrives tagged "<home>". It is
        // dropped, not painted over the new active slot's redraw (blocker 2).
        assert_eq!(
            route_slot_event(HOME_SWITCH_TARGET, "anvil", false),
            SlotRouting::Drop
        );
    }

    #[test]
    fn route_non_active_slot_death_demotes_not_drops() {
        // A non-active slot's lifecycle death (reader disconnect / ServerShutdown)
        // demotes that slot — it never tears the active session down (blocker 1).
        assert_eq!(
            route_slot_event("anvil", HOME_SWITCH_TARGET, true),
            SlotRouting::DemoteDead
        );
    }

    #[test]
    fn route_active_slot_death_applies_connection_lost() {
        // The active slot's death routes to Apply — the loop then returns
        // ConnectionLost, today's semantics for the slot driving the terminal.
        assert_eq!(route_slot_event("anvil", "anvil", true), SlotRouting::Apply);
    }

    /// A warm slot's transport dying (its socketpair peer closes) must demote
    /// the slot in the manager+registry while the ACTIVE slot is untouched —
    /// the session survives (blocker 1). The loop drives this by routing the
    /// dead warm reader's `ServerDisconnected` to `handle_dead`; here we invoke
    /// `handle_dead` directly after killing the peer, asserting the registry
    /// state the session depends on.
    #[test]
    fn warm_slot_death_demotes_and_session_survives() {
        let (home_local, _home_peer) = UnixStream::pair().unwrap();
        let mut manager = SlotManager::new(
            SlotConnection {
                target: SlotTarget::Home,
                write_stream: home_local,
            },
            vec![ssh("anvil")],
            8,
        );
        // Warm anvil over its own socketpair.
        let (anvil_local, anvil_peer) = UnixStream::pair().unwrap();
        manager
            .add_warm(SlotConnection {
                target: ssh("anvil"),
                write_stream: anvil_local,
            })
            .unwrap();
        assert_eq!(manager.registry.phase(&ssh("anvil")), Some(SlotPhase::Warm));

        // Kill the warm slot's transport: drop its peer end (EOF on the reader).
        drop(anvil_peer);
        // The loop's reaction to that reader's ServerDisconnected:
        manager.handle_dead(&ssh("anvil"));

        // The warm slot is demoted to cold; the ACTIVE (home) slot is intact —
        // the session did NOT tear down.
        assert_eq!(
            manager.registry.phase(&ssh("anvil")),
            Some(SlotPhase::Cold { failed_at: None })
        );
        assert_eq!(manager.registry.active_target(), Some(&SlotTarget::Home));
        // A later switch to the dead slot re-dials it (cold fallback).
        assert_eq!(
            manager.registry.request_switch(&ssh("anvil")),
            SwitchOutcome::ColdDial(ssh("anvil"))
        );
    }

    #[test]
    fn flip_to_cold_slot_returns_none_for_relaunch_fallback() {
        let (home_local, _home_peer) = UnixStream::pair().unwrap();
        let mut manager = SlotManager::new(
            SlotConnection {
                target: SlotTarget::Home,
                write_stream: home_local,
            },
            vec![ssh("anvil")],
            8,
        );
        // anvil is cold (never warmed): flip falls back to the dial/leg path.
        assert!(manager.flip_to(&ssh("anvil")).unwrap().is_none());
    }
}
