//! Host link registry: pure state for the local server's links to remote
//! herdr servers. Transport will live in host_transport.rs (Task 5); this
//! module holds no I/O, mirroring the AppState/PaneRuntime separation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct HostLinkId(pub(crate) String); // ssh config alias, e.g. "workbox"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Offline,
}

#[derive(Debug)]
pub(crate) struct HostLink {
    // Not read yet outside Debug output; iter() consumers arrive in Task 5/6.
    #[allow(dead_code)]
    pub(crate) id: HostLinkId,
    pub(crate) state: LinkState,
}

#[derive(Debug, Default)]
pub(crate) struct HostLinkRegistry {
    links: BTreeMap<HostLinkId, HostLink>,
}

pub(crate) const MAX_RECONNECT_ATTEMPTS: u32 = 6;

impl HostLinkRegistry {
    // Not called outside tests yet; the host transport (Task 5) and adoption
    // (Task 6) work will drive these through the real connection lifecycle.
    #[allow(dead_code)]
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

    #[allow(dead_code)] // see attach() above; consumed starting Task 5/6
    pub(crate) fn detach(&mut self, id: &HostLinkId) -> bool {
        self.links.remove(id).is_some()
    }

    #[allow(dead_code)] // see attach() above; consumed starting Task 5/6
    pub(crate) fn on_connected(&mut self, id: &HostLinkId) {
        if let Some(link) = self.links.get_mut(id) {
            link.state = LinkState::Connected;
        }
    }

    /// Connection dropped. Returns the next state (drives transport retry).
    ///
    /// Offline is terminal for automatic retries; manual retry is modeled as
    /// detach + attach.
    #[allow(dead_code)] // see attach() above; consumed starting Task 5/6
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

    #[allow(dead_code)] // see attach() above; consumed starting Task 5/6
    pub(crate) fn state(&self, id: &HostLinkId) -> Option<LinkState> {
        self.links.get(id).map(|l| l.state)
    }

    #[allow(dead_code)] // see attach() above; consumed starting Task 5/6
    pub(crate) fn iter(&self) -> impl Iterator<Item = &HostLink> {
        self.links.values()
    }
}

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
