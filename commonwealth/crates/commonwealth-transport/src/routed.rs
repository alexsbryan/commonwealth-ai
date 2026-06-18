// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-traffic-class transport routing with automatic IP fallback
//! (Track W3 of TRANSPORT_MIGRATION.md). Composes N transports behind
//! the single [`PeerTransport`] seam: each [`TrafficClass`] routes to a
//! chosen transport, whose candidates are CONCATENATED ahead of the
//! default transport's.
//!
//! Call sites already try candidates in order and stop at the first
//! success, so per-dial fallback to the default (IP) path is free and
//! automatic: a failed iroh dial degrades to the tailnet path on the
//! SAME request, with no config flip and no call-site change. This is
//! the ~20-line composition the seam was designed for — a flip is a
//! change to which class maps to which transport, nothing else.
//!
//! Unrouted classes (no override) resolve through the default alone,
//! so a `RoutedTransport` whose `per_class` is empty is behaviourally
//! identical to its default — the daemon only installs one once at
//! least one class is flipped.

use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::ids::NodeId;

use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

/// Routes each [`TrafficClass`] to a transport, falling back to
/// `default` (the IP overlay) per dial.
#[derive(Debug)]
pub struct RoutedTransport {
    per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>>,
    default: Arc<dyn PeerTransport>,
}

impl RoutedTransport {
    pub fn new(
        per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>>,
        default: Arc<dyn PeerTransport>,
    ) -> Self {
        Self { per_class, default }
    }

    /// The primary transport for a class: the per-class override, else
    /// the default.
    fn primary(&self, class: TrafficClass) -> &Arc<dyn PeerTransport> {
        self.per_class.get(&class).unwrap_or(&self.default)
    }

    /// Every distinct transport this router holds, for `note_success`
    /// dispatch. Small N (≤ traffic classes + 1).
    fn transports(&self) -> impl Iterator<Item = &Arc<dyn PeerTransport>> {
        std::iter::once(&self.default).chain(self.per_class.values())
    }
}

#[async_trait::async_trait]
impl PeerTransport for RoutedTransport {
    fn name(&self) -> &'static str {
        "routed"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        let primary = self.primary(class);
        let mut out = primary.endpoints(peer, class).await;
        // Fallback: for a class routed to a non-default transport,
        // append the default's candidates AFTER the primary's, so a
        // caller trying in order degrades to IP automatically when the
        // primary dial fails. Skipped when primary IS the default
        // (Arc identity) to avoid double-listing.
        if !Arc::ptr_eq(primary, &self.default) {
            out.extend(self.default.endpoints(peer, class).await);
        }
        out
    }

    fn note_success(&self, peer: NodeId, class: TrafficClass, endpoint: &PeerEndpoint) {
        // Deliver the feedback to the transport that produced this
        // endpoint, identified by its label prefix (`ip:` / `iroh:`).
        // Mis-delivery is harmless (IpTransport's `contains` guard
        // no-ops a foreign address) but correct delivery keeps the
        // last-working-address promotion meaningful.
        let prefix = endpoint.label.split(':').next().unwrap_or("");
        for t in self.transports() {
            if t.name() == prefix {
                t.note_success(peer, class, endpoint);
                return;
            }
        }
        // Unknown prefix: notify the class's primary as a fallback.
        self.primary(class).note_success(peer, class, endpoint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A transport with a fixed name, canned candidates, and a record
    /// of which endpoints `note_success` was called with.
    #[derive(Debug)]
    struct Mock {
        nm: &'static str,
        eps: Vec<PeerEndpoint>,
        hits: Mutex<Vec<String>>,
    }

    impl Mock {
        fn new(nm: &'static str, labels: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                nm,
                eps: labels.iter().map(|l| ep(l)).collect(),
                hits: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl PeerTransport for Mock {
        fn name(&self) -> &'static str {
            self.nm
        }
        async fn endpoints(&self, _p: &PeerContact, _c: TrafficClass) -> Vec<PeerEndpoint> {
            self.eps.clone()
        }
        fn note_success(&self, _p: NodeId, _c: TrafficClass, e: &PeerEndpoint) {
            self.hits.lock().unwrap().push(e.label.clone());
        }
    }

    fn ep(label: &str) -> PeerEndpoint {
        PeerEndpoint {
            base_url: format!("http://{label}"),
            label: label.to_string(),
        }
    }

    fn contact() -> PeerContact {
        PeerContact {
            node_id: NodeId::from_u128(1),
            addresses: vec![],
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: vec![],
        }
    }

    fn labels(eps: Vec<PeerEndpoint>) -> Vec<String> {
        eps.into_iter().map(|e| e.label).collect()
    }

    #[tokio::test]
    async fn unrouted_class_uses_default_only() {
        let ip = Mock::new("ip", &["ip:a"]);
        let routed = RoutedTransport::new(HashMap::new(), ip);
        let out = routed.endpoints(&contact(), TrafficClass::Gossip).await;
        assert_eq!(labels(out), vec!["ip:a"]);
    }

    #[tokio::test]
    async fn routed_class_lists_primary_then_default_fallback() {
        let iroh = Mock::new("iroh", &["iroh:x"]);
        let ip = Mock::new("ip", &["ip:y"]);
        let mut per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>> = HashMap::new();
        per_class.insert(TrafficClass::Gossip, iroh);
        let routed = RoutedTransport::new(per_class, ip);

        // Gossip is flipped → iroh candidate FIRST, then the IP
        // fallback — the automatic per-dial degrade.
        let g = routed.endpoints(&contact(), TrafficClass::Gossip).await;
        assert_eq!(labels(g), vec!["iroh:x", "ip:y"]);

        // Inference is NOT flipped → IP only, no iroh.
        let i = routed.endpoints(&contact(), TrafficClass::Inference).await;
        assert_eq!(labels(i), vec!["ip:y"]);
    }

    #[tokio::test]
    async fn note_success_routes_to_the_transport_that_produced_the_endpoint() {
        let iroh = Mock::new("iroh", &["iroh:x"]);
        let ip = Mock::new("ip", &["ip:y"]);
        let mut per_class: HashMap<TrafficClass, Arc<dyn PeerTransport>> = HashMap::new();
        per_class.insert(TrafficClass::Gossip, iroh.clone());
        let routed = RoutedTransport::new(per_class, ip.clone());

        let peer = NodeId::from_u128(1);
        routed.note_success(peer, TrafficClass::Gossip, &ep("iroh:x"));
        routed.note_success(peer, TrafficClass::Gossip, &ep("ip:y"));

        assert_eq!(iroh.hits.lock().unwrap().clone(), vec!["iroh:x"]);
        assert_eq!(ip.hits.lock().unwrap().clone(), vec!["ip:y"]);
    }
}
