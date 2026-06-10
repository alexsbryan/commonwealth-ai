// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end test of the `/internal/join` handshake — the path
//! `sovereign-mesh::join::perform_join` and `EmbeddedDaemon::join_mesh`
//! ultimately drive.
//!
//! `gossip_integration.rs` covers the *merge* (two AppStates with
//! pre-seeded members converging); this file covers the **admission**
//! (a fresh joiner POSTs a raw join key, the founder verifies it
//! against `mesh.join_key_hash`, mints a new `MemberRecord`, and
//! returns the full mesh snapshot). Two failure modes worth pinning:
//!
//! 1. **Happy path:** valid `join_key` → 200 with assigned NodeId +
//!    mesh containing both members; the founder's AppState mutates
//!    and the mesh-mutation hook fires.
//! 2. **Auth boundary:** wrong `join_key` → 401, no mutation. The
//!    timing-attack-resistant equality lives in
//!    `membership::verify_join_key`; this test pins that the
//!    HTTP-level rejection path stays wired.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::Mesh;
use commonwealth_discovery::membership;

async fn spawn_internal_router(state: AppState) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, internal_router(state)).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

/// Build a founder `AppState` from a freshly-initialised mesh +
/// install a counter-incrementing mesh-mutation hook so the test can
/// assert the hook fires on `/internal/join` acceptance. Returns the
/// state, the founder's `NodeId`, the plaintext `join_key`, and the
/// hook counter.
fn build_founder() -> (AppState, NodeId, String, Arc<AtomicUsize>) {
    let founder_id = NodeId::from_u128(0xF0F0_F0F0_F0F0_F0F0);
    let founder_addr: SocketAddr = "127.0.0.1:9742".parse().unwrap();
    let (mesh, join_key) =
        membership::init_mesh_with_node_id("Test Mesh", "Founder", vec![founder_addr], founder_id);
    let state = AppState::new(founder_id, mesh);

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    let hook: commonwealth_api::state::MeshMutationHook =
        Arc::new(move |_mesh: &Mesh, _self_id: NodeId| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });
    let state = state.with_mesh_mutation_hook(hook);

    (state, founder_id, join_key, counter)
}

#[tokio::test]
async fn valid_join_key_admits_new_member_and_fires_hook() {
    let (founder_state, founder_id, join_key, hook_counter) = build_founder();
    let addr = spawn_internal_router(founder_state.clone()).await;

    // Pre-condition: founder is alone, hook hasn't fired.
    assert_eq!(founder_state.inner.mesh.read().await.members.len(), 1);
    assert_eq!(hook_counter.load(Ordering::Relaxed), 0);

    let joiner_addr: SocketAddr = "127.0.0.1:9876".parse().unwrap();
    let proposed_id = NodeId::from_u128(0x5050_5050_5050_5050);

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/join"))
        .json(&json!({
            "join_key": join_key,
            "joining_node_name": "Joiner",
            "joining_node_addresses": [joiner_addr],
            "proposed_node_id": proposed_id,
        }))
        .send()
        .await
        .expect("/internal/join must be reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "valid join key must be admitted"
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let assigned_id_value = body
        .get("assigned_node_id")
        .expect("response must carry assigned_node_id");
    assert!(!assigned_id_value.is_null(), "assigned_node_id present");

    // The returned mesh snapshot must contain both members. MeshWire
    // flattens to `members: Vec<MemberRecord>` for transport.
    let members = body["mesh"]["members"]
        .as_array()
        .expect("mesh.members must serialise as an array");
    assert_eq!(
        members.len(),
        2,
        "snapshot must include founder + joiner: {body}"
    );
    let names: Vec<&str> = members
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        names.contains(&"Founder"),
        "founder absent in response: {names:?}"
    );
    assert!(
        names.contains(&"Joiner"),
        "joiner absent in response: {names:?}"
    );

    // The founder's live AppState mesh must now contain the joiner
    // — proves the handshake actually mutated state, not just
    // returned a synthesized response.
    let live = founder_state.inner.mesh.read().await;
    assert_eq!(
        live.members.len(),
        2,
        "founder AppState must show both members after handshake"
    );
    assert!(live.members.values().any(|m| m.name == "Joiner"));
    drop(live);

    // The mesh-mutation hook fired exactly once for the admission.
    // Regression target: if `with_mesh_mutation_hook` ever gets
    // re-ordered after an `Arc::clone(&app_state.inner)`, this
    // counter stays at zero and on-join persistence falls back to
    // the 10-second gossip-loop cadence (silent failure today).
    assert_eq!(
        hook_counter.load(Ordering::Relaxed),
        1,
        "mutation hook must fire once on /internal/join admission"
    );

    // Sanity on founder identity: even though the test passed a
    // `proposed_node_id`, the founder's own NodeId should be
    // unchanged. The handshake admits the joiner; it doesn't
    // re-stamp the founder.
    assert_eq!(
        founder_state
            .inner
            .self_node_id_swap
            .load_full()
            .as_ref()
            .clone(),
        founder_id,
        "founder NodeId must not change under /internal/join"
    );
}

#[tokio::test]
async fn join_with_pubkey_and_valid_proof_records_identity() {
    use commonwealth_transport::identity;

    let (founder_state, _founder_id, join_key, _hook) = build_founder();
    let addr = spawn_internal_router(founder_state.clone()).await;

    let joiner_addr: SocketAddr = "127.0.0.1:9878".parse().unwrap();
    let proposed_id = NodeId::from_u128(0x6060_6060_6060_6060);
    let key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
    let pubkey = identity::node_pubkey(&key);
    let proof = identity::sign_join_proof(&key, &proposed_id, "KeyedJoiner");

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/join"))
        .json(&json!({
            "join_key": join_key,
            "joining_node_name": "KeyedJoiner",
            "joining_node_addresses": [joiner_addr],
            "proposed_node_id": proposed_id,
            "node_pubkey": pubkey,
            "pubkey_proof": proof,
        }))
        .send()
        .await
        .expect("/internal/join must be reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // The founder's live mesh records the joiner WITH its pubkey —
    // the identity that a future dial-by-key transport dials.
    let live = founder_state.inner.mesh.read().await;
    let joiner = live
        .members
        .values()
        .find(|m| m.name == "KeyedJoiner")
        .expect("joiner admitted");
    assert_eq!(
        joiner.node_pubkey,
        Some(pubkey),
        "founder must record the proven identity pubkey"
    );
}

#[tokio::test]
async fn join_with_pubkey_but_bad_proof_is_rejected_401() {
    use commonwealth_transport::identity;

    let (founder_state, _founder_id, join_key, hook_counter) = build_founder();
    let addr = spawn_internal_router(founder_state.clone()).await;

    let joiner_addr: SocketAddr = "127.0.0.1:9879".parse().unwrap();
    let proposed_id = NodeId::from_u128(0x7070_7070_7070_7070);
    let key = ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]);
    // Proof signed by a DIFFERENT key than the presented pubkey —
    // the imposter scenario the PoP exists to block.
    let other = ed25519_dalek::SigningKey::from_bytes(&[43u8; 32]);
    let proof = identity::sign_join_proof(&other, &proposed_id, "Imposter");

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/join"))
        .json(&json!({
            "join_key": join_key,
            "joining_node_name": "Imposter",
            "joining_node_addresses": [joiner_addr],
            "proposed_node_id": proposed_id,
            "node_pubkey": identity::node_pubkey(&key),
            "pubkey_proof": proof,
        }))
        .send()
        .await
        .expect("/internal/join must be reachable");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "unproven pubkey must be a loud 401, never a silent admit"
    );
    assert_eq!(
        founder_state.inner.mesh.read().await.members.len(),
        1,
        "rejected join must not mutate the founder's mesh"
    );
    assert_eq!(hook_counter.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn invalid_join_key_rejects_with_401_and_does_not_mutate() {
    let (founder_state, _founder_id, _real_key, hook_counter) = build_founder();
    let addr = spawn_internal_router(founder_state.clone()).await;

    let joiner_addr: SocketAddr = "127.0.0.1:9877".parse().unwrap();
    // A well-formed but wrong key. Format check passes in
    // `validate_join_key_format` (so we get past the deserialiser);
    // the BLAKE3 comparison against `mesh.join_key_hash` then fails.
    let bad_key = "cwth-AAAA-BBBB-CCCC";

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/join"))
        .json(&json!({
            "join_key": bad_key,
            "joining_node_name": "Imposter",
            "joining_node_addresses": [joiner_addr],
        }))
        .send()
        .await
        .expect("/internal/join must be reachable for the auth-failure case");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "wrong join_key must be rejected with 401, not silently merged"
    );

    // No mutation, no hook fire. The auth-failure path must short
    // -circuit before any Mesh write.
    assert_eq!(
        founder_state.inner.mesh.read().await.members.len(),
        1,
        "rejected join must not mutate the founder's mesh"
    );
    assert_eq!(
        hook_counter.load(Ordering::Relaxed),
        0,
        "rejected join must not fire the mutation hook"
    );
}

/// Two-instance integration: after a successful handshake, the
/// joiner can adopt the founder's mesh snapshot and the two
/// `AppState`s match. This is the path
/// `sovereign-mesh::join::perform_join` drives end-to-end on a real
/// daemon — exercising the JSON serialise/deserialise round-trip
/// through `MeshWire` (the `HashMap<NodeId, MemberRecord>` → `Vec`
/// flattening that the wire shape uses to avoid JSON key issues).
#[tokio::test]
async fn joiner_can_adopt_founder_mesh_after_handshake() {
    let (founder_state, _founder_id, join_key, _hook) = build_founder();
    let addr = spawn_internal_router(founder_state.clone()).await;

    let joiner_addr: SocketAddr = "127.0.0.1:9878".parse().unwrap();
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/join"))
        .json(&json!({
            "join_key": join_key,
            "joining_node_name": "Joiner",
            "joining_node_addresses": [joiner_addr],
        }))
        .send()
        .await
        .expect("/internal/join reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Reconstruct the joiner's `Mesh` from the wire snapshot — same
    // shape `sovereign-mesh::join` produces when it adopts the
    // founder's view. If `MeshWire::into_mesh` ever loses a member
    // or peers list in transit, this assertion is the canary.
    let body: serde_json::Value = resp.json().await.unwrap();
    let members = body["mesh"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.get("name").and_then(|n| n.as_str()).unwrap_or("?"))
        .collect::<Vec<_>>();
    let mut sorted = members.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec!["Founder", "Joiner"],
        "wire-shaped snapshot must include both members exactly once: {members:?}"
    );

    // The founder's mesh, queried via its internal AppState (not
    // the wire), must match the wire shape. This is the
    // round-trip-equivalence check that catches drift between
    // `Mesh` and `MeshWire`.
    let live = founder_state.inner.mesh.read().await;
    let mut live_names: Vec<&str> = live.members.values().map(|m| m.name.as_str()).collect();
    live_names.sort();
    assert_eq!(
        live_names, sorted,
        "founder's AppState mesh must match the wire snapshot served to the joiner"
    );

    // The mesh_id and join_key_hash must also round-trip — the
    // joiner uses these to authorise subsequent gossip rounds.
    // `Mesh::merge_from` rejects gossip from a peer whose mesh_id
    // or join_key_hash doesn't match, so drift here would silently
    // brick the joiner's gossip loop after a successful handshake.
    let wire_mesh_id = body["mesh"]["id"].clone();
    let wire_hash = body["mesh"]["join_key_hash"].clone();
    let live_mesh_id = serde_json::to_value(live.id).unwrap();
    let live_hash = serde_json::to_value(live.join_key_hash).unwrap();
    assert_eq!(wire_mesh_id, live_mesh_id, "mesh_id round-trips");
    assert_eq!(wire_hash, live_hash, "join_key_hash round-trips");
    drop(live);

    // Sanity: AppState construction works for the joiner too. We
    // don't run a second internal_router here — gossip convergence
    // is already covered by gossip_integration.rs — but instantiating
    // proves the wire-shape consumption path doesn't panic on the
    // adopted mesh.
    let wire_members: Vec<commonwealth_core::mesh::MemberRecord> =
        serde_json::from_value(body["mesh"]["members"].clone()).expect("members must deserialise");
    let mut hm = HashMap::new();
    for m in wire_members {
        hm.insert(m.node_id, m);
    }
    let joiner_mesh = Mesh {
        id: serde_json::from_value(body["mesh"]["id"].clone()).unwrap(),
        name: body["mesh"]["name"].as_str().unwrap().to_string(),
        join_key_hash: serde_json::from_value(body["mesh"]["join_key_hash"].clone()).unwrap(),
        members: hm,
        peers: serde_json::from_value(body["mesh"]["peers"].clone()).unwrap_or_default(),
    };
    let joiner_id_value = body["assigned_node_id"].clone();
    let joiner_id: NodeId =
        serde_json::from_value(joiner_id_value).expect("assigned_node_id must deserialise");
    let _joiner_state = AppState::new(joiner_id, joiner_mesh);
}
