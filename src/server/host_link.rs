//! Host link registry: pure state for the local server's links to remote
//! herdr servers. Transport will live in host_transport.rs (Task 5); this
//! module holds no I/O, mirroring the AppState/PaneRuntime separation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// This module itself is cross-platform (pure state, no I/O), but its only
// caller -- the multi-host host-event consumer in src/server/headless.rs --
// is `#[cfg(unix)]`-gated (the ssh transport it drives lives in
// src/remote/unix.rs, per the plan's "Windows support out of scope" note).
// So every item here is genuinely unreachable on a Windows build; silence
// that specifically instead of unconditionally, so a real regression to
// unix-only reachability would still be caught there.
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct HostLinkId(pub(crate) String); // ssh config alias, e.g. "workbox"

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Offline,
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct HostLink {
    pub(crate) id: HostLinkId,
    pub(crate) state: LinkState,
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Default)]
pub(crate) struct HostLinkRegistry {
    links: BTreeMap<HostLinkId, HostLink>,
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const MAX_RECONNECT_ATTEMPTS: u32 = 6;

#[cfg_attr(not(unix), allow(dead_code))]
impl HostLinkRegistry {
    // Driven by the server's host-event consumer (src/server/headless.rs,
    // Task 9): `host.attach` calls this before spawning the transport.
    pub(crate) fn attach(&mut self, id: HostLinkId) -> Result<(), AttachError> {
        if self.links.contains_key(&id) {
            return Err(AttachError::AlreadyAttached);
        }
        self.links.insert(
            id.clone(),
            HostLink {
                id,
                state: LinkState::Connecting,
            },
        );
        Ok(())
    }

    pub(crate) fn detach(&mut self, id: &HostLinkId) -> bool {
        self.links.remove(id).is_some()
    }

    pub(crate) fn on_connected(&mut self, id: &HostLinkId) {
        if let Some(link) = self.links.get_mut(id) {
            link.state = LinkState::Connected;
        }
    }

    /// Connection dropped. Returns the next state (drives transport retry).
    ///
    /// Offline is terminal for automatic retries; manual retry is modeled as
    /// detach + attach.
    pub(crate) fn on_disconnect(&mut self, id: &HostLinkId) -> Option<LinkState> {
        let link = self.links.get_mut(id)?;
        link.state = match link.state {
            LinkState::Connecting | LinkState::Connected => LinkState::Reconnecting { attempt: 1 },
            LinkState::Reconnecting { attempt } if attempt < MAX_RECONNECT_ATTEMPTS => {
                LinkState::Reconnecting {
                    attempt: attempt + 1,
                }
            }
            LinkState::Reconnecting { .. } | LinkState::Offline => LinkState::Offline,
        };
        Some(link.state)
    }

    pub(crate) fn state(&self, id: &HostLinkId) -> Option<LinkState> {
        self.links.get(id).map(|l| l.state)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &HostLink> {
        self.links.values()
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AttachError {
    AlreadyAttached,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> HostLinkId {
        HostLinkId(s.to_string())
    }

    #[test]
    fn attach_starts_connecting_and_duplicate_attach_is_rejected() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("workbox")).unwrap();
        assert_eq!(r.state(&id("workbox")), Some(LinkState::Connecting));
        assert_eq!(r.attach(id("workbox")), Err(AttachError::AlreadyAttached));
    }

    #[test]
    fn disconnect_backs_off_then_goes_offline() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        r.on_connected(&id("wb"));
        assert_eq!(r.state(&id("wb")), Some(LinkState::Connected));
        for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
            let next = r.on_disconnect(&id("wb")).unwrap();
            assert_eq!(next, LinkState::Reconnecting { attempt });
        }
        assert_eq!(r.on_disconnect(&id("wb")).unwrap(), LinkState::Offline);
    }

    #[test]
    fn reconnect_success_resets_attempt_counter() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        r.on_connected(&id("wb"));
        r.on_disconnect(&id("wb"));
        r.on_connected(&id("wb"));
        assert_eq!(
            r.on_disconnect(&id("wb")).unwrap(),
            LinkState::Reconnecting { attempt: 1 }
        );
    }

    #[test]
    fn detach_removes_link_and_offline_links_never_auto_retry() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        // MAX_RECONNECT_ATTEMPTS reconnect attempts, then one more lands Offline.
        for _ in 0..=MAX_RECONNECT_ATTEMPTS {
            r.on_disconnect(&id("wb"));
        }
        assert_eq!(r.state(&id("wb")), Some(LinkState::Offline));
        // Offline links never auto retry: further disconnects stay Offline.
        assert_eq!(r.on_disconnect(&id("wb")), Some(LinkState::Offline));
        assert!(r.detach(&id("wb")));
        assert_eq!(r.state(&id("wb")), None);
        assert!(!r.detach(&id("wb")));
    }

    #[test]
    fn initial_connect_failure_backs_off() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        assert_eq!(
            r.on_disconnect(&id("wb")).unwrap(),
            LinkState::Reconnecting { attempt: 1 }
        );
    }

    #[test]
    fn unknown_id_is_inert() {
        let mut r = HostLinkRegistry::default();
        r.on_connected(&id("ghost"));
        assert_eq!(r.on_disconnect(&id("ghost")), None);
        assert_eq!(r.state(&id("ghost")), None);
    }

    #[test]
    fn late_connect_after_offline_recovers() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        for _ in 0..=MAX_RECONNECT_ATTEMPTS {
            r.on_disconnect(&id("wb"));
        }
        assert_eq!(r.state(&id("wb")), Some(LinkState::Offline));
        r.on_connected(&id("wb"));
        assert_eq!(r.state(&id("wb")), Some(LinkState::Connected));
        assert_eq!(
            r.on_disconnect(&id("wb")).unwrap(),
            LinkState::Reconnecting { attempt: 1 }
        );
    }

    #[test]
    fn attach_after_detach_starts_fresh() {
        let mut r = HostLinkRegistry::default();
        r.attach(id("wb")).unwrap();
        for _ in 0..3 {
            r.on_disconnect(&id("wb"));
        }
        assert_eq!(
            r.state(&id("wb")),
            Some(LinkState::Reconnecting { attempt: 3 })
        );
        assert!(r.detach(&id("wb")));
        r.attach(id("wb")).unwrap();
        assert_eq!(r.state(&id("wb")), Some(LinkState::Connecting));
        assert_eq!(
            r.on_disconnect(&id("wb")).unwrap(),
            LinkState::Reconnecting { attempt: 1 }
        );
    }
}
