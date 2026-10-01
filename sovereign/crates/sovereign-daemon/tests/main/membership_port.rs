// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon reads membership through ONE port (pb-mesh-exit-core).
//!
//! Two halves. The routes that read the roster answer from whatever
//! `MembershipReader` Fabric was built with, not from the in-process `Mesh`
//! beside it: `/status` and the knowledge fan-out are driven against a double
//! whose roster differs from the state's own. And no route or loop reads
//! `fabric.mesh` directly: the only files that may are the mesh endpoint and
//! the membership verbs the flip (pb-mesh-exit-transport) deletes, each held
//! to its count today. A direct read that names no type is invisible to a
//! grep for `commonwealth_core::mesh::`, so the guard greps the field.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use kernel_types::NodeId;
use mesh_reach::PeerContact;
use oicp_types::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use oicp_types::knowledge::CorpusShardInfo;
use oicp_types::FederatedMeshDescriptor;
use sovereign_contracts::daemon_wire::MemberStatus;
use sovereign_contracts::membership::{MembershipEntry, MembershipReader};
use sovereign_daemon::server::client_router;
use sovereign_daemon::state::FabricSeed;
use sovereign_daemon::state::{test_app_state_with_seed, AppState};
use tower::ServiceExt;

/// The files that may still read `fabric.mesh`, and how many lines each:
/// none. The endpoint that read it (create/join/leave/rotate, the mesh proof,
/// the gossip and join handlers) went with pb-mesh-exit-transport, and the
/// roster is cw-rails', read through `AppState::membership()`. A read that
/// appears here belongs behind that port.
const ENDPOINT_READS: &[(&str, usize)] = &[];

struct RosterDouble {
    name: &'static str,
    members: Vec<MembershipEntry<PeerContact>>,
}

#[async_trait]
impl MembershipReader for RosterDouble {
    type Dial = PeerContact;

    async fn mesh_name(&self) -> String {
        self.name.to_string()
    }

    async fn federated_meshes(&self) -> Vec<FederatedMeshDescriptor> {
        Vec::new()
    }

    async fn members(&self) -> Vec<MembershipEntry<PeerContact>> {
        self.members.clone()
    }
}

fn entry(
    id: u128,
    name: &str,
    status: MemberStatus,
    active: bool,
    free_vram_gb: f32,
    corpora: &[&str],
    addr: SocketAddr,
) -> MembershipEntry<PeerContact> {
    let node_id = NodeId::from_u128(id);
    MembershipEntry {
        node_id,
        name: name.to_string(),
        status,
        active,
        last_seen: 100,
        dialable: true,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 16,
                cpu_cores: 8,
                total_storage_gb: 500,
                free_storage_gb: 200,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources {
                free_vram_gb,
                ..AvailableResources::default()
            },
            active_processes: vec![],
            hosted_corpora: corpora
                .iter()
                .map(|c| CorpusShardInfo {
                    corpus_id: c.to_string(),
                    chunk_range: None,
                    is_replica: false,
                    last_updated: 100,
                    chunk_count: 1,
                    canonical_fingerprint: None,
                    total_shards: None,
                    processed_shards: vec![],
                    atlas_atom_count: 0,
                    atlas_tier2_count: 0,
                    atlas_fingerprint: None,
                })
                .collect(),
            reported_at: 100,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],
            origins: Vec::new(),
            media_allow: Vec::new(),
            media_available: None,
            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
            storage_remaining_bytes: None,
        },
        dial: PeerContact {
            node_id,
            addresses: vec![addr],
            node_pubkey: None,
            relay_url: None,
            iroh_direct_addrs: Vec::new(),
        },
    }
}

/// A state whose own `Mesh` is `test_app_state`'s ("Test Mesh", no members)
/// and whose membership port is `double`.
fn state_over(double: RosterDouble) -> AppState {
    test_app_state_with_seed(FabricSeed {
        peer_transport: sovereign_daemon::double::address_transport(),
        membership: Some(Arc::new(double)),
        ..Default::default()
    })
}

async fn call(state: AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let mut req = req;
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
    ));
    let response = client_router(state).oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn status_counts_the_mesh_from_the_membership_port() {
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = state_over(RosterDouble {
        name: "Double Mesh",
        members: vec![
            entry(1, "self", MemberStatus::Online, true, 1.0, &[], addr),
            entry(2, "online", MemberStatus::Online, true, 8.0, &[], addr),
            entry(3, "busy", MemberStatus::Busy, true, 100.0, &[], addr),
            entry(4, "offline", MemberStatus::Offline, true, 100.0, &[], addr),
            entry(5, "departed", MemberStatus::Online, false, 16.0, &[], addr),
        ],
    });

    let (status, body) = call(state, Request::get("/status").body(Body::empty()).unwrap()).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let mesh = &body["mesh"];
    assert_eq!(
        mesh["name"], "Double Mesh",
        "the name is the port's: {mesh}"
    );
    // Active and Online|Busy: self, online, busy. The departed row is Online
    // but a tombstone.
    assert_eq!(mesh["members_online"], 3, "{mesh}");
    // Active rows: everyone but the departed one.
    assert_eq!(mesh["members_total"], 4, "{mesh}");
    // Online rows, tombstones included (the rule the route had before the
    // port): 1 + 8 + 16.
    assert_eq!(mesh["pooled_vram_gb"], 25.0, "{mesh}");
}

#[tokio::test]
async fn the_knowledge_fanout_plans_from_the_membership_port() {
    // The only member hosting `sep` exists in the port, not in the state's
    // own Mesh. An unconstrained search reaches it only by reading the port;
    // nothing listens at its address, so the corpus comes back unavailable.
    let dead: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = state_over(RosterDouble {
        name: "Double Mesh",
        members: vec![
            entry(1, "self", MemberStatus::Online, true, 0.0, &[], dead),
            entry(2, "Remote", MemberStatus::Online, true, 0.0, &["sep"], dead),
        ],
    });
    let request = serde_json::json!({
        "query_embedding": vec![0.0_f32; 8],
        "query_text": "anything",
        "limit": 4,
    });
    let req = Request::post("/v1/knowledge/search")
        .header("content-type", "application/json")
        .body(Body::from(request.to_string()))
        .unwrap();

    let (status, body) = call(state, req).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let unavailable = body["corpora_unavailable"].as_array().unwrap();
    assert!(
        unavailable.iter().any(|c| c == "sep"),
        "the port's peer was planned and dialled: {body}"
    );
}

fn endpoint_reads(src: &Path) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for item in std::fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                // Test trees build fixtures on the Mesh; they are not reads.
                if path.file_name().is_some_and(|n| n != "tests") {
                    stack.push(path);
                }
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let reads = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| l.contains("fabric.mesh") && !l.trim_start().starts_with("//"))
                .count();
            if reads > 0 {
                let rel = path
                    .strip_prefix(src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(rel, reads);
            }
        }
    }
    found
}

#[test]
fn only_the_mesh_endpoint_reads_fabric_mesh_directly() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = endpoint_reads(&src);
    let allowed: BTreeMap<String, usize> = ENDPOINT_READS
        .iter()
        .map(|(f, n)| (f.to_string(), *n))
        .collect();
    assert_eq!(
        found, allowed,
        "a route or loop reads `fabric.mesh` directly; read membership through \
         `AppState::membership()` (pb-mesh-exit-core). Left: what src/ holds; \
         right: the endpoint's reads the flip deletes."
    );
}
