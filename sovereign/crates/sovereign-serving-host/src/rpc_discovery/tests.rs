// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for RPC-worker discovery's endpoint policy — see `rpc_discovery.rs`
//! (moved with it from the daemon's `tests/daemon.rs`).

use super::*;

#[test]
fn sticky_holds_direct_ip_through_transient_misses_then_flips() {
    // The 2026-07-19 flap in miniature: a proven direct-ip must NOT flip to
    // the bridge on one miss — hold it until the threshold, THEN flip.
    let flip = 3;
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
    // Miss 1: bridge offered, but hold direct-ip.
    let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s1.endpoint, "10.0.0.5:50052", "must not flip on one miss");
    assert!(s1.is_direct());
    assert_eq!(s1.direct_misses, 1);
    // Miss 2: still holding (2 < 3).
    let s2 = sticky_endpoint(Some(&s1), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s2.endpoint, "10.0.0.5:50052");
    assert_eq!(s2.direct_misses, 2);
    // Miss 3 reaches the threshold — NOW accept the bridge (durable change).
    let s3 = sticky_endpoint(Some(&s2), bridge("127.0.0.1:40001"), flip).unwrap();
    assert_eq!(s3.endpoint, "127.0.0.1:40001");
    assert_eq!(s3.via, "iroh-bridge:x");
    assert_eq!(s3.direct_misses, 0);
}

#[test]
fn sticky_direct_ip_recovery_resets_miss_count() {
    let flip = 3;
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), flip).unwrap();
    let s1 = sticky_endpoint(Some(&s0), None, flip).unwrap(); // total miss → hold
    assert_eq!(s1.direct_misses, 1);
    // Direct-ip answers again → back to a clean slate.
    let s2 = sticky_endpoint(Some(&s1), direct("10.0.0.5:50052"), flip).unwrap();
    assert!(s2.is_direct());
    assert_eq!(s2.direct_misses, 0);
}

#[test]
fn sticky_drops_a_non_direct_worker_when_unreachable() {
    // A bridge-only worker (no proven direct-ip to protect) is dropped the
    // moment it's unreachable — nothing to hold.
    let bridge_only = StickyEndpoint {
        endpoint: "127.0.0.1:1".to_string(),
        via: "iroh-bridge:x".to_string(),
        direct_misses: 0,
    };
    assert!(sticky_endpoint(Some(&bridge_only), None, 3).is_none());
}

fn held(via: &str) -> StickyEndpoint {
    StickyEndpoint {
        endpoint: "127.0.0.1:40021".to_string(),
        via: via.to_string(),
        direct_misses: 0,
    }
}

#[test]
fn reaffirm_probes_only_what_it_has_never_seen() {
    use RpcTunnelMode::*;
    // First sight of a peer: nothing held, so the full probe is the only way
    // to learn whether it serves an RPC worker at all.
    assert_eq!(reaffirm_plan(None, Auto), Reaffirm::FullProbe);
    // A proven direct-ip is re-affirmed from cache (2026-07-19 guard).
    assert_eq!(
        reaffirm_plan(Some(&held("direct-ip")), Auto),
        Reaffirm::Held
    );
    // A probe-host fallback is a last resort, not evidence of anything —
    // keep re-probing so it can be promoted to a real transport.
    assert_eq!(
        reaffirm_plan(Some(&held("probe-host")), Auto),
        Reaffirm::FullProbe
    );
}

#[test]
fn reaffirm_never_reprobes_a_known_bridged_worker_over_its_own_tunnel() {
    // THE 2026-07-26 REGRESSION. A bridged worker was re-probed via
    // `/status` every tick; that probe rides the same iroh path as the
    // tunnel, so under load it timed out, `fresh` went None, and
    // `sticky_endpoint` drops a non-direct endpoint on a miss (asserted in
    // `sticky_drops_a_non_direct_worker_when_unreachable`) — which the
    // eligibility tracker reads as a flap. Observed: endpoint pinned at
    // 127.0.0.1:40021 for six minutes while flaps climbed to 9 and the
    // cooldown compounded to 300s, excluding a peer that was serving.
    for via in ["iroh-bridge:x", "iroh-bridge:iroh:127.0.0.1:40021→86627fd5"] {
        assert_eq!(
            reaffirm_plan(Some(&held(via)), RpcTunnelMode::Auto),
            Reaffirm::Rebridge,
            "{via} must be re-minted from the local bridge cache, never re-probed"
        );
        assert_eq!(
            reaffirm_plan(Some(&held(via)), RpcTunnelMode::Always),
            Reaffirm::Rebridge
        );
    }
}

#[test]
fn reaffirm_respects_an_operator_opting_out_of_bridging() {
    // `SOVEREIGN_RPC_TUNNEL=never` withdraws permission to tunnel. Holding a
    // bridge endpoint would pin the worker to a transport we may no longer
    // use, so re-probe: it either surfaces at a direct address or drops out.
    assert_eq!(
        reaffirm_plan(Some(&held("iroh-bridge:x")), RpcTunnelMode::Never),
        Reaffirm::FullProbe
    );
    // The direct-ip hold is unaffected by the tunnel knob.
    assert_eq!(
        reaffirm_plan(Some(&held("direct-ip")), RpcTunnelMode::Never),
        Reaffirm::Held
    );
}

#[test]
fn sticky_flip_threshold_one_disables_the_hold() {
    // threshold 1 = flip on the first miss (the pre-guard behaviour), so the
    // env knob's floor is a conscious opt-out, not a silent no-op.
    let s0 = sticky_endpoint(None, direct("10.0.0.5:50052"), 1).unwrap();
    let s1 = sticky_endpoint(Some(&s0), bridge("127.0.0.1:2"), 1).unwrap();
    assert_eq!(s1.endpoint, "127.0.0.1:2", "threshold 1 flips immediately");
}

#[test]
fn rpc_tunnel_mode_parses_the_documented_values() {
    use RpcTunnelMode::*;
    assert_eq!(rpc_tunnel_mode_from(None), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("")), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("auto")), Auto);
    assert_eq!(rpc_tunnel_mode_from(Some("ALWAYS")), Always);
    assert_eq!(rpc_tunnel_mode_from(Some(" always ")), Always);
    assert_eq!(rpc_tunnel_mode_from(Some("never")), Never);
    assert_eq!(rpc_tunnel_mode_from(Some("off")), Never);
    assert_eq!(rpc_tunnel_mode_from(Some("0")), Never);
    // Unknown values degrade to the safe default, never panic.
    assert_eq!(rpc_tunnel_mode_from(Some("banana")), Auto);
}

#[test]
fn rpc_endpoint_directory_records_and_resolves() {
    // The warm orchestrator resolves worker identity through this
    // directory; an unknown endpoint (env-configured worker) is None so
    // callers fall back to raw-IP addressing.
    let discovery = RpcWorkerDiscovery::default();
    assert_eq!(discovery.endpoint_node("10.0.0.7:50052"), None);

    let node = NodeId::from_u128(42);
    discovery
        .rpc_endpoint_nodes
        .write()
        .unwrap()
        .insert("10.0.0.7:50052".to_string(), node);
    assert_eq!(discovery.endpoint_node("10.0.0.7:50052"), Some(node));
    // Re-discovery overwrites in place — same endpoint, later owner wins.
    let other = NodeId::from_u128(43);
    discovery
        .rpc_endpoint_nodes
        .write()
        .unwrap()
        .insert("10.0.0.7:50052".to_string(), other);
    assert_eq!(discovery.endpoint_node("10.0.0.7:50052"), Some(other));
}

fn direct(ep: &str) -> Option<(String, String)> {
    Some((ep.to_string(), "direct-ip".to_string()))
}
fn bridge(ep: &str) -> Option<(String, String)> {
    Some((ep.to_string(), "iroh-bridge:x".to_string()))
}

#[test]
fn sticky_takes_fresh_direct_ip_immediately() {
    // First-ever sight of a verified direct-ip: no prior, take it, misses=0.
    let s = sticky_endpoint(None, direct("10.0.0.9:50052"), 3).unwrap();
    assert_eq!(s.endpoint, "10.0.0.9:50052");
    assert!(s.is_direct());
    assert_eq!(s.direct_misses, 0);
}

/// A roster of one peer, as the membership port hands it.
struct OnePeer(sovereign_contracts::membership::MembershipEntry<PeerContact>);

#[async_trait::async_trait]
impl MembershipReader for OnePeer {
    type Dial = PeerContact;
    async fn mesh_name(&self) -> String {
        "fixture".to_string()
    }
    async fn federated_meshes(&self) -> Vec<oicp_types::FederatedMeshDescriptor> {
        Vec::new()
    }
    async fn members(&self) -> Vec<sovereign_contracts::membership::MembershipEntry<PeerContact>> {
        vec![self.0.clone()]
    }
}

/// A transport with no path to anyone: no `/status` probe endpoint and no
/// bridge, as for a peer on cw-rails' roster, which has no daemon `/status`.
#[derive(Debug)]
struct Nowhere;

#[async_trait::async_trait]
impl PeerTransport for Nowhere {
    fn name(&self) -> &'static str {
        "none"
    }
    async fn endpoints(&self, _: &PeerContact, _: TrafficClass) -> Vec<mesh_reach::PeerEndpoint> {
        Vec::new()
    }
}

/// Discovery reads a worker's port from the roster's anchor record
/// (pb-serve-distributes), so a worker on a roster with no `/status` behind
/// it is found and dialled at its direct address. Failing input: read the
/// port only from the peer's `/status`, and this tick discovers nothing.
#[tokio::test]
async fn a_worker_the_roster_names_is_discovered_without_a_status_probe() {
    let worker = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = worker.local_addr().expect("addr").port();
    let peer = NodeId::from_u128(9);
    let capabilities: oicp_types::capabilities::NodeCapabilities =
        serde_json::from_value(serde_json::json!({
            "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                         "total_storage_gb": 0, "free_storage_gb": 0},
            "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                          "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                          "available_for_mesh": true},
            "hosted_corpora": [], "reported_at": 0,
            "anchor": {"can_anchor": true, "vram_gb": 0, "rpc_port": port}
        }))
        .expect("capabilities");
    let roster = OnePeer(sovereign_contracts::membership::MembershipEntry {
        node_id: peer,
        name: "worker".to_string(),
        status: MemberStatus::Online,
        active: true,
        last_seen: 0,
        dialable: true,
        capabilities,
        dial: PeerContact {
            node_id: peer,
            addresses: vec!["127.0.0.1:9742".parse().unwrap()],
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
        },
    });
    let transport: Arc<dyn PeerTransport> = Arc::new(Nowhere);
    let outcome = RpcWorkerDiscovery::default()
        .discover(&roster, &transport, NodeId::from_u128(1))
        .await;
    let found: Vec<(NodeId, String)> = outcome
        .workers
        .iter()
        .map(|w| (w.node_id, w.endpoint.clone()))
        .collect();
    assert_eq!(found, vec![(peer, format!("127.0.0.1:{port}"))]);
    assert!(outcome.scanned);
    assert_eq!(outcome.polled, 1);
}
