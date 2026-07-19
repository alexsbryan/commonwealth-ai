// SPDX-License-Identifier: AGPL-3.0-or-later
//! The IP-overlay transport — today's production path.
//!
//! Reaches peers over whatever IP connectivity exists (Tailscale,
//! LAN, WireGuard). All Tailscale-specific knowledge — the CGNAT /
//! ULA address ranking in `commonwealth_core::peer_addr` — is
//! consumed HERE and nowhere else: it is IP-overlay knowledge, and
//! a dial-by-identity transport never sees it.
//!
//! Port policy per traffic class, chosen to byte-reproduce the URL
//! strings the call sites built inline before the seam existed:
//!
//! | class | policy |
//! |---|---|
//! | Gossip, ControlPlane, KnowledgeSearch, ModelTransfer | gossiped `SocketAddr` verbatim (it carries the peer's *internal* port — that's what the join handshake advertised) |
//! | Inference, StatusProbe | gossiped host, port rewritten to this mesh's `client_port` |
//!
//! The client-port rewrite carries the uniform-port assumption
//! documented at `EmbeddedDaemon::peer_inference_endpoints`: every
//! peer's client API is assumed to listen on the same `client_port`
//! this daemon exposes locally. Mixed-port meshes need a
//! `MemberRecord.client_port` wire field — until then operators who
//! change `client_port` must change it everywhere.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use commonwealth_core::ids::NodeId;

use crate::{PeerContact, PeerEndpoint, PeerTransport, TrafficClass};

/// Default client-API port, matching `DaemonConfig.node.api_port`.
pub const DEFAULT_CLIENT_PORT: u16 = 9741;

/// IP-overlay implementation of [`PeerTransport`].
#[derive(Debug)]
pub struct IpTransport {
    client_port: u16,
    /// Last `SocketAddr` that worked, per peer. Promoted to the
    /// front of the candidate list on the next resolution so a peer
    /// whose best-ranked address is unreachable (a stale LAN IP
    /// shadowed by a working Tailscale address) doesn't burn a
    /// timeout every round. Successor of gossip's process-global
    /// `last_working_address_cache`; lives on the transport now,
    /// which has the same effective lifetime (one per daemon run).
    /// Best-effort: a stored address that stopped working slows
    /// down one resolution, then the next success rewrites it.
    last_working: Mutex<HashMap<NodeId, SocketAddr>>,
}

impl Default for IpTransport {
    fn default() -> Self {
        Self::new(DEFAULT_CLIENT_PORT)
    }
}

impl IpTransport {
    /// `client_port` is this mesh's (assumed-uniform) client API
    /// port, used for the Inference/StatusProbe port rewrite.
    pub fn new(client_port: u16) -> Self {
        Self {
            client_port,
            last_working: Mutex::new(HashMap::new()),
        }
    }

    fn rewritten_port(&self, class: TrafficClass) -> Option<u16> {
        match class {
            TrafficClass::Inference | TrafficClass::StatusProbe => Some(self.client_port),
            TrafficClass::Gossip
            | TrafficClass::ControlPlane
            | TrafficClass::KnowledgeSearch
            | TrafficClass::ModelTransfer
            | TrafficClass::RpcTensor => None,
        }
    }
}

#[async_trait::async_trait]
impl PeerTransport for IpTransport {
    fn name(&self) -> &'static str {
        "ip"
    }

    async fn endpoints(&self, peer: &PeerContact, class: TrafficClass) -> Vec<PeerEndpoint> {
        // RpcTensor rides per-worker RPC ports advertised via `/status`,
        // not the uniform mesh ports this transport knows. Returning a
        // wrong-port guess would burn the caller's connect budget on an
        // address that can never answer — no candidates is the honest
        // response; discovery's own probing owns the raw-TCP path.
        if class == TrafficClass::RpcTensor {
            return Vec::new();
        }
        let mut addrs = commonwealth_core::peer_addr::sorted_addresses(&peer.addresses);

        // Promote the last-working address to the front, preserving
        // the ranked order of the rest. Exactly the semantics the
        // gossip loop had: the hint only applies when the cached
        // address is still in the peer's gossiped list.
        if let Some(preferred) = self
            .last_working
            .lock()
            .ok()
            .and_then(|c| c.get(&peer.node_id).copied())
        {
            if addrs.contains(&preferred) {
                addrs.retain(|a| *a != preferred);
                addrs.insert(0, preferred);
            }
        }

        let endpoints: Vec<PeerEndpoint> = addrs
            .iter()
            .map(|addr| {
                let base_url = match self.rewritten_port(class) {
                    // `SocketAddr`'s Display brackets IPv6 for us:
                    // "100.64.0.2:9742" / "[fd7a::1]:9742".
                    None => format!("http://{addr}"),
                    Some(port) => {
                        let ip = addr.ip();
                        if ip.is_ipv6() {
                            format!("http://[{ip}]:{port}")
                        } else {
                            format!("http://{ip}:{port}")
                        }
                    }
                };
                let label = format!("ip:{}", &base_url["http://".len()..]);
                PeerEndpoint { base_url, label }
            })
            .collect();

        tracing::debug!(
            target: "transport",
            transport = self.name(),
            class = class.as_str(),
            peer = %peer.node_id,
            candidates = endpoints.len(),
            first = endpoints.first().map(|e| e.label.as_str()).unwrap_or("-"),
            "transport: resolved"
        );
        endpoints
    }

    fn note_success(&self, peer: NodeId, _class: TrafficClass, endpoint: &PeerEndpoint) {
        // Only verbatim-class endpoints round-trip to a gossiped
        // `SocketAddr`. Port-rewritten endpoints (Inference /
        // StatusProbe) parse fine but carry the client port, which
        // never matches the gossiped list — the `contains` guard in
        // `endpoints()` makes such an entry a no-op rather than a
        // wrong promotion.
        if let Some(authority) = endpoint.base_url.strip_prefix("http://") {
            if let Ok(addr) = authority.parse::<SocketAddr>() {
                if let Ok(mut cache) = self.last_working.lock() {
                    cache.insert(peer, addr);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_contact;
    use commonwealth_core::mesh::MemberRecord;

    /// The realistic multi-homed fixture: a peer behind tailnet +
    /// LAN, gossiped in "wrong" order, internal port 9742.
    ///
    /// Deserialized from pre-identity wire JSON (no `node_pubkey`
    /// key) so this fixture doubles as the old→new wire-compat
    /// witness: a record gossiped by a pre-identity build must
    /// parse with `node_pubkey: None`.
    fn fixture_member() -> MemberRecord {
        let member: MemberRecord = serde_json::from_value(serde_json::json!({
            "node_id": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,7],
            "name": "peer",
            "invited_by": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,1],
            "joined_at": 0,
            "last_seen": 0,
            "status": "online",
            "capabilities": {
                "hardware": {
                    "gpus": [],
                    "system_ram_gb": 0,
                    "cpu_cores": 0,
                    "total_storage_gb": 0,
                    "free_storage_gb": 0,
                    "network_bandwidth_mbps": null
                },
                "available": {
                    "free_vram_gb": 0.0,
                    "free_ram_gb": 0.0,
                    "free_storage_gb": 0.0,
                    "gpu_utilization": 0.0,
                    "cpu_utilization": 0.0,
                    "available_for_mesh": true
                },
                "active_processes": [],
                "hosted_corpora": [],
                "reported_at": 0
            },
            "addresses": [
                "[fd7a:115c:a1e0::a3a:241c]:9742",
                "100.64.0.2:9742",
                "192.168.1.42:9742"
            ]
        }))
        .expect("pre-identity wire JSON must deserialize");
        assert!(member.node_pubkey.is_none());
        member
    }

    fn urls(eps: &[PeerEndpoint]) -> Vec<&str> {
        eps.iter().map(|e| e.base_url.as_str()).collect()
    }

    /// Golden vectors: verbatim classes must yield exactly the
    /// strings the pre-seam call sites built with
    /// `format!("http://{addr}")` — ranked Tailscale-IPv4 / LAN-IPv4
    /// / ULA-IPv6, internal port preserved, IPv6 bracketed.
    #[tokio::test]
    async fn golden_verbatim_classes() {
        let t = IpTransport::default();
        let contact = peer_contact(&fixture_member());
        for class in [
            TrafficClass::Gossip,
            TrafficClass::ControlPlane,
            TrafficClass::KnowledgeSearch,
            TrafficClass::ModelTransfer,
        ] {
            let eps = t.endpoints(&contact, class).await;
            assert_eq!(
                urls(&eps),
                vec![
                    "http://100.64.0.2:9742",
                    "http://192.168.1.42:9742",
                    "http://[fd7a:115c:a1e0::a3a:241c]:9742",
                ],
                "class {class:?}"
            );
        }
    }

    /// Golden vectors: Inference must reproduce
    /// `peer_inference_endpoints`' output — host kept, port
    /// rewritten to client_port, `/v1` appended by the call site.
    #[tokio::test]
    async fn golden_inference_port_rewrite() {
        let t = IpTransport::new(9741);
        let contact = peer_contact(&fixture_member());
        let eps = t.endpoints(&contact, TrafficClass::Inference).await;
        let with_path: Vec<String> = eps.iter().map(|e| format!("{}/v1", e.base_url)).collect();
        assert_eq!(
            with_path,
            vec![
                "http://100.64.0.2:9741/v1",
                "http://192.168.1.42:9741/v1",
                "http://[fd7a:115c:a1e0::a3a:241c]:9741/v1",
            ]
        );
    }

    /// StatusProbe matches `discover_rpc_workers`' URL shape.
    #[tokio::test]
    async fn golden_status_probe() {
        let t = IpTransport::new(9741);
        let contact = peer_contact(&fixture_member());
        let eps = t.endpoints(&contact, TrafficClass::StatusProbe).await;
        assert_eq!(
            eps[0].base_url.clone() + "/status",
            "http://100.64.0.2:9741/status"
        );
    }

    /// note_success must reproduce the gossip cache semantics: the
    /// last-working address goes first on the next resolution, the
    /// remaining addresses keep ranked order.
    #[tokio::test]
    async fn note_success_promotes_to_front() {
        let t = IpTransport::default();
        let contact = peer_contact(&fixture_member());
        let eps = t.endpoints(&contact, TrafficClass::Gossip).await;
        // Pretend the ULA (ranked last) was the one that worked.
        t.note_success(contact.node_id, TrafficClass::Gossip, &eps[2]);
        let eps2 = t.endpoints(&contact, TrafficClass::Gossip).await;
        assert_eq!(
            urls(&eps2),
            vec![
                "http://[fd7a:115c:a1e0::a3a:241c]:9742",
                "http://100.64.0.2:9742",
                "http://192.168.1.42:9742",
            ]
        );
    }

    /// A cached address that left the gossiped list must NOT be
    /// promoted (same `contains` guard the gossip loop had).
    #[tokio::test]
    async fn stale_cache_entry_is_ignored() {
        let t = IpTransport::default();
        let mut member = fixture_member();
        let contact = peer_contact(&member);
        let eps = t.endpoints(&contact, TrafficClass::Gossip).await;
        t.note_success(contact.node_id, TrafficClass::Gossip, &eps[1]);
        // Peer re-gossips without the LAN address.
        member.addresses = vec![
            "[fd7a:115c:a1e0::a3a:241c]:9742".parse().unwrap(),
            "100.64.0.2:9742".parse().unwrap(),
        ];
        let eps2 = t
            .endpoints(&peer_contact(&member), TrafficClass::Gossip)
            .await;
        assert_eq!(
            urls(&eps2),
            vec![
                "http://100.64.0.2:9742",
                "http://[fd7a:115c:a1e0::a3a:241c]:9742",
            ]
        );
    }

    /// Port-rewritten endpoints reported via note_success never
    /// poison the promotion (client-port addr fails the contains
    /// guard).
    #[tokio::test]
    async fn rewritten_port_success_does_not_poison_promotion() {
        let t = IpTransport::new(9741);
        let contact = peer_contact(&fixture_member());
        let inf = t.endpoints(&contact, TrafficClass::Inference).await;
        t.note_success(contact.node_id, TrafficClass::Inference, &inf[0]);
        let gossip = t.endpoints(&contact, TrafficClass::Gossip).await;
        assert_eq!(gossip[0].base_url, "http://100.64.0.2:9742");
    }

    #[tokio::test]
    async fn empty_addresses_yield_no_endpoints() {
        let t = IpTransport::default();
        let mut member = fixture_member();
        member.addresses.clear();
        let eps = t
            .endpoints(&peer_contact(&member), TrafficClass::Gossip)
            .await;
        assert!(eps.is_empty());
    }

    #[tokio::test]
    async fn labels_carry_transport_prefix() {
        let t = IpTransport::default();
        let contact = peer_contact(&fixture_member());
        let eps = t.endpoints(&contact, TrafficClass::Gossip).await;
        assert_eq!(eps[0].label, "ip:100.64.0.2:9742");
    }
}
