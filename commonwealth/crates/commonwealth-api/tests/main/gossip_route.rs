// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for the `/internal/gossip` push-pull handler.
//!
//! Verifies that two `AppState` instances on the same mesh (same
//! `mesh_id` + `invite_key_hash`) can POST their `Mesh` at each other
//! and end up with a unioned member view — the mechanic that
//! converges persisted-but-diverged peers.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::capabilities::{AvailableResources, HardwareProfile, NodeCapabilities};
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh, NodeStatus};
use tower::ServiceExt;

fn member(id: NodeId, name: &str, last_seen: u64) -> MemberRecord {
    MemberRecord {
        removed_at: None,
        node_pubkey: None,
        relay_url: None,
        iroh_direct_addrs: Vec::new(),
        dial_info_version: 0,
        dial_info_sig: None,
        node_id: id,
        name: name.into(),
        invited_by: id,
        joined_at: 0,
        last_seen,
        status: NodeStatus::Online,
        capabilities: NodeCapabilities {
            hardware: HardwareProfile {
                gpus: vec![],
                system_ram_gb: 0,
                cpu_cores: 0,
                total_storage_gb: 0,
                free_storage_gb: 0,
                network_bandwidth_mbps: None,
            },
            available: AvailableResources::default(),
            active_processes: vec![],
            hosted_corpora: vec![],
            reported_at: last_seen,
            inference_availability: 1.0,
            inference_capable: false,
            loaded_models: vec![],

            embed_model: None,
            benchmark: None,
            current_in_flight: None,
            anchor: None,
        },
        addresses: vec!["192.168.1.1:9742".parse::<SocketAddr>().unwrap()],
    }
}

fn mesh_with(mesh_id: MeshId, hash: [u8; 32], members: Vec<MemberRecord>) -> Mesh {
    let mut map = HashMap::new();
    for m in members {
        map.insert(m.node_id, m);
    }
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Test".into(),
        invite_key_hash: hash,
        invite_version: 0,
        require_encryption: false,
        members: map,
        peers: vec![],
    }
}

/// Wire-format mirror of `commonwealth_api::routes_internal::MeshWire`
/// so tests can build request bodies without the type being public.
/// Keeps flat-Vec members so serde_json doesn't choke on NodeId keys.
#[derive(serde::Serialize)]
struct MeshWireBody<'a> {
    id: MeshId,
    name: &'a str,
    /// Wire name is historical; the Rust field was renamed in the credential
    /// split. Mirrors `routes_internal::MeshWire` — if these drift the route
    /// 422s, which is how this mirror earns its keep.
    #[serde(rename = "join_key_hash")]
    invite_key_hash: [u8; 32],
    #[serde(default)]
    invite_version: u64,
    require_encryption: bool,
    members: Vec<MemberRecord>,
    peers: Vec<commonwealth_core::mesh::MeshPeering>,
}

fn gossip_request_body(mesh: &Mesh) -> serde_json::Value {
    let wire = MeshWireBody {
        id: mesh.id,
        name: &mesh.name,
        invite_key_hash: mesh.invite_key_hash,
        invite_version: mesh.invite_version,
        require_encryption: false,
        members: mesh.members.values().cloned().collect(),
        peers: mesh.peers.clone(),
    };
    serde_json::json!({ "mesh": wire })
}

/// The upgraded caller's body: identifies itself, offers a proof, and may or
/// may not still be shipping the raw secret depending on whether it has
/// confirmed us. `mesh_secret` is added as a sibling key rather than a
/// `MeshWireBody` field so the pre-split builder above stays byte-identical.
fn gossip_request_body_proving(
    mesh: &Mesh,
    from: NodeId,
    proof: &str,
    send_raw_secret: Option<[u8; 32]>,
) -> serde_json::Value {
    let mut body = gossip_request_body(mesh);
    if let Some(secret) = send_raw_secret {
        body["mesh"]["mesh_secret"] = serde_json::json!(secret.to_vec());
    }
    body["from"] = serde_json::to_value(from).unwrap();
    body["mesh_proof"] = serde_json::json!(proof);
    body
}

/// Reads `mesh.mesh_secret` out of a gossip reply, absent counting as zeroed.
fn replied_secret(resp: &serde_json::Value) -> Vec<u64> {
    resp["mesh"]["mesh_secret"]
        .as_array()
        .map(|bytes| bytes.iter().map(|b| b.as_u64().unwrap_or(0)).collect())
        .unwrap_or_default()
}

async fn post_gossip(state: &AppState, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// The request half of P4b stopped putting the raw secret on the wire; this is
/// the reply half. A caller that PROVED possession does not need the
/// credential back, so answering with it leaves it on the LAN every 10s
/// between two fully upgraded nodes — the exact exposure the proof exists to
/// remove.
///
/// The discriminating case is a peer that proves AND still ships its secret:
/// that is every upgraded pair's first round, and every round until each side
/// has confirmed the other.
#[tokio::test]
async fn an_upgraded_caller_gets_no_raw_secret_back() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let caller = NodeId::from_u128(2);
    let secret = [42u8; 32];

    let mut local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    local.mesh_secret = secret;
    let state = AppState::new(node_a, local.clone());

    let incoming = mesh_with(mesh_id, hash, vec![member(caller, "Caller", 200)]);
    let proof = local
        .mesh_proof(caller, commonwealth_core::clock::unix_now_secs())
        .unwrap();
    let (status, resp) = post_gossip(
        &state,
        gossip_request_body_proving(&incoming, caller, &proof, Some(secret)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let returned = replied_secret(&resp);
    assert!(
        returned.iter().all(|b| *b == 0),
        "the reply handed the raw mesh_secret to a caller that had already \
         proved it holds one: {returned:?}"
    );
    assert_eq!(
        state.inner.mesh.read().await.mesh_secret,
        secret,
        "redacting the reply must not clobber the live secret"
    );
}

/// A peer that proves possession is post-split by definition, whatever its
/// payload carried. Once the outbound path withholds the secret from confirmed
/// peers, reading the payload alone reports an upgraded peer as pre-split —
/// which blocks `rotate_invite` naming it, and makes us resume sending the
/// credential we had just stopped sending.
#[tokio::test]
async fn a_proving_caller_that_withholds_its_secret_is_recorded_post_split() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let caller = NodeId::from_u128(2);

    let mut local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    local.mesh_secret = [42u8; 32];
    let state = AppState::new(node_a, local.clone());

    // Withholding: no `mesh_secret` on the wire at all, only the proof.
    let incoming = mesh_with(mesh_id, hash, vec![member(caller, "Caller", 200)]);
    let proof = local
        .mesh_proof(caller, commonwealth_core::clock::unix_now_secs())
        .unwrap();
    let (status, _) = post_gossip(
        &state,
        gossip_request_body_proving(&incoming, caller, &proof, None),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        state.peer_confirmed_post_split(caller),
        "a proving peer must be recorded post-split; recording it pre-split is \
         what blocks rotation between two upgraded nodes"
    );
}

/// The compat half, unchanged: a pre-split caller is still admitted and still
/// recorded as pre-split, so `rotate_invite` still refuses rather than
/// partitioning it.
#[tokio::test]
async fn a_pre_split_caller_is_still_recorded_pre_split() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let caller = NodeId::from_u128(2);

    let mut local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    local.mesh_secret = [42u8; 32];
    let state = AppState::new(node_a, local);

    let incoming = mesh_with(mesh_id, hash, vec![member(caller, "Caller", 200)]);
    let mut body = gossip_request_body(&incoming);
    body["from"] = serde_json::to_value(caller).unwrap();
    let (status, _) = post_gossip(&state, body).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        !state.peer_confirmed_post_split(caller),
        "a caller with neither proof nor secret is pre-split, and rotation must \
         keep refusing while it is online"
    );
}

#[tokio::test]
async fn gossip_merges_incoming_member_into_local_view() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1); // self
    let node_b = NodeId::from_u128(2); // will be learned via gossip

    // Local mesh: only `A` knows about itself.
    let local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    let state = AppState::new(node_a, local);

    // Incoming: a peer's view that includes B.
    let incoming = mesh_with(
        mesh_id,
        hash,
        vec![member(node_a, "A", 100), member(node_b, "B", 200)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&incoming)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // The response echoes OUR updated view, now containing both A and B.
    let returned_members = resp["mesh"]["members"].as_array().unwrap();
    assert_eq!(returned_members.len(), 2);

    // And the AppState itself was mutated.
    let mesh = state.inner.mesh.read().await;
    assert!(mesh.members.contains_key(&node_a));
    assert!(mesh.members.contains_key(&node_b));
}

#[tokio::test]
async fn gossip_rejects_wrong_mesh_id() {
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let local = mesh_with(MeshId::from_u128(1), hash, vec![member(node_a, "A", 10)]);
    let state = AppState::new(node_a, local);

    let foreign = mesh_with(
        MeshId::from_u128(999), // different mesh!
        hash,
        vec![member(NodeId::from_u128(99), "Intruder", 9999)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&foreign)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let mesh = state.inner.mesh.read().await;
    assert_eq!(mesh.members.len(), 1, "reject must not mutate");
}

#[tokio::test]
async fn gossip_rejects_mismatched_invite_key_hash() {
    let mesh_id = MeshId::from_u128(1);
    let node_a = NodeId::from_u128(1);
    let local = mesh_with(mesh_id, [3u8; 32], vec![member(node_a, "A", 10)]);
    let state = AppState::new(node_a, local);

    let fake = mesh_with(
        mesh_id,
        [9u8; 32], // attacker knows mesh_id but not invite_key_hash
        vec![member(NodeId::from_u128(99), "Intruder", 9999)],
    );

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&fake)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn gossip_does_not_overwrite_self_record() {
    // A buggy or malicious peer ships us a stale view of ourselves
    // (wrong name, Offline status). We're authoritative for self.
    let mesh_id = MeshId::from_u128(1);
    let hash = [3u8; 32];
    let me = NodeId::from_u128(1);

    let local = mesh_with(mesh_id, hash, vec![member(me, "Real-Me", 100)]);
    let state = AppState::new(me, local);

    let bogus_view_of_self = {
        let mut m = member(me, "Wrong-Name", 999999);
        m.status = NodeStatus::Offline;
        m
    };
    let incoming = mesh_with(mesh_id, hash, vec![bogus_view_of_self]);

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&incoming)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mesh = state.inner.mesh.read().await;
    let my_record = mesh.members.get(&me).unwrap();
    assert_eq!(my_record.name, "Real-Me");
    assert_eq!(my_record.status, NodeStatus::Online);
    assert_eq!(my_record.last_seen, 100);
}

/// A caller that omits `mesh_secret` picks the LEGACY predicate — it only has
/// to know `invite_key_hash`, which rides every gossip payload and every join
/// snapshot, and which a departed member still holds (the mesh has no
/// eviction). The reply must not hand back the real secret.
///
/// `mesh_secret` never rotates and `rotate_invite_key` is structurally unable
/// to change it, so disclosure here is permanent and unrevocable — strictly
/// worse than the pre-split model, where rotating the key DID revoke.
#[tokio::test]
async fn a_legacy_authorized_caller_cannot_read_our_mesh_secret() {
    let mesh_id = MeshId::from_u128(7);
    let hash = [3u8; 32];
    let node_a = NodeId::from_u128(1);
    let caller = NodeId::from_u128(2);

    // We are fully post-split: a real secret is set.
    let mut local = mesh_with(mesh_id, hash, vec![member(node_a, "A", 100)]);
    local.mesh_secret = [42u8; 32];
    let state = AppState::new(node_a, local);

    // The caller knows the invite hash and simply omits mesh_secret, which is
    // what `gossip_request_body` produces (it never sets the field).
    let incoming = mesh_with(mesh_id, hash, vec![member(caller, "Caller", 200)]);

    let app = internal_router(state.clone());
    let response = app
        .oneshot(
            Request::post("/internal/gossip")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&gossip_request_body(&incoming)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a pre-split peer must still be admitted — that is the whole point of \
         the compat arm; this test is about what comes BACK"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let returned = resp["mesh"]["mesh_secret"].as_array();
    if let Some(bytes) = returned {
        let bytes: Vec<u64> = bytes.iter().map(|b| b.as_u64().unwrap_or(0)).collect();
        assert!(
            bytes.iter().all(|b| *b == 0),
            "the reply leaked mesh_secret to a legacy-authorized caller: {bytes:?}"
        );
    }

    // And our own secret is untouched — redaction is on the wire, not a mutation.
    assert_eq!(
        state.inner.mesh.read().await.mesh_secret,
        [42u8; 32],
        "redacting the reply must not clobber the live secret"
    );
}
