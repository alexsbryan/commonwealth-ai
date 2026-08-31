// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gossip auth-boundary test.
//!
//! `Mesh::merge_from` is the auth boundary for the `/internal/gossip`
//! endpoint: any incoming payload whose `mesh_id` or `invite_key_hash`
//! doesn't match ours is rejected wholesale (`MergeReport.rejected ==
//! true`), and the route handler at `routes_internal::gossip` turns
//! that into a `401 Unauthorized` response with no Mesh write.
//!
//! Why this needs a test: the `MergeReport.rejected` flag is set in
//! one place (`commonwealth-core::mesh::Mesh::merge_from`) and read in
//! another (`commonwealth-api::routes_internal::gossip`). A future
//! refactor that adds a code path between the auth check and the
//! Mesh write — or that silently turns the reject into a 200 — would
//! create the §7 invariant slip "anyone who knows mesh_id (public via
//! mDNS) but not the join_key can inject members into our view." The
//! test pins:
//!
//! 1. **Wrong `mesh_id` → 401, no mutation, no hook fire.**
//! 2. **Wrong `invite_key_hash` → 401, no mutation, no hook fire.**
//! 3. **Matching id + hash + a new member → 200, mutation visible,
//!    hook fires.** (Negative control proves the test isn't just
//!    accepting every 401.)
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use commonwealth_api::server::internal_router;
use commonwealth_api::state::AppState;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::{MemberRecord, Mesh};

use crate::common;
use crate::common::{member_with_last_seen as member, spawn_router};

/// Build a founder AppState pinned to (mesh_id, invite_key_hash) +
/// install a counter-incrementing mesh-mutation hook so the test
/// can assert the hook does NOT fire on rejected payloads.
fn build_founder(
    mesh_id: MeshId,
    invite_key_hash: [u8; 32],
) -> (AppState, NodeId, Arc<AtomicUsize>) {
    let founder_id = NodeId::from_u128(0xCAFE_BABE_CAFE_BABE);
    let mut members = HashMap::new();
    members.insert(
        founder_id,
        member(
            founder_id,
            "Founder",
            100,
            "127.0.0.1:9742".parse().unwrap(),
        ),
    );
    let mesh = Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: mesh_id,
        name: "Auth Test".into(),
        invite_key_hash,
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    };
    let state = AppState::new(founder_id, mesh);

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&counter);
    let hook: commonwealth_api::state::MeshMutationHook =
        Arc::new(move |_mesh: &Mesh, _self_id: NodeId| {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        });
    let state = state.with_mesh_mutation_hook(hook);

    (state, founder_id, counter)
}

async fn spawn_internal(state: AppState) -> SocketAddr {
    spawn_router(internal_router(state)).await
}

/// Build a MeshWire-shaped JSON payload from explicit parameters,
/// so each test can drift one field at a time.
fn gossip_payload(
    mesh_id: MeshId,
    invite_key_hash: [u8; 32],
    members: Vec<MemberRecord>,
) -> serde_json::Value {
    json!({
        "mesh": {
            "id": mesh_id,
            "name": "any name",
            "join_key_hash": invite_key_hash.to_vec(),
            "members": members,
            "peers": Vec::<serde_json::Value>::new(),
        }
    })
}

#[tokio::test]
async fn wrong_mesh_id_rejects_with_401_and_no_mutation() {
    // Founder is in mesh A; we send a gossip payload tagged as mesh B
    // with a new member. The auth check inside `Mesh::merge_from`
    // (mesh_id mismatch) sets `rejected = true`, the route returns 401,
    // and the new member must NOT appear in the founder's view.
    let mesh_a = MeshId::from_u128(0xA0A0_A0A0);
    let mesh_b = MeshId::from_u128(0xB0B0_B0B0);
    let hash = [0x77; 32];

    let (state, founder_id, hook_counter) = build_founder(mesh_a, hash);
    let addr = spawn_internal(state.clone()).await;

    let intruder = NodeId::from_u128(0xBAD_BAD_BAD_BAD);
    let payload = gossip_payload(
        mesh_b, // ← mismatching mesh_id
        hash,
        vec![
            member(
                founder_id,
                "Founder",
                100,
                "127.0.0.1:9742".parse().unwrap(),
            ),
            member(
                intruder,
                "Intruder",
                200,
                "192.168.1.99:9742".parse().unwrap(),
            ),
        ],
    );

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/gossip"))
        .json(&payload)
        .send()
        .await
        .expect("internal/gossip reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "wrong mesh_id must yield 401, got {}",
        resp.status()
    );

    // No mutation on auth failure.
    let mesh = state.inner.mesh.read().await;
    assert_eq!(
        mesh.members.len(),
        1,
        "rejected gossip must NOT add the intruder; got members: {:?}",
        mesh.members.keys().collect::<Vec<_>>()
    );
    assert!(
        !mesh.members.contains_key(&intruder),
        "intruder NodeId leaked into the founder's mesh state"
    );
    drop(mesh);

    assert_eq!(
        hook_counter.load(Ordering::Relaxed),
        0,
        "mutation hook fired on a 401 — that would persist the unauthorised state to mesh.json"
    );
}

#[tokio::test]
async fn wrong_invite_key_hash_rejects_with_401_and_no_mutation() {
    // Same shape as above but mesh_id matches; the auth check
    // catches the invite_key_hash mismatch instead. Tests the
    // second half of `Mesh::merge_from`'s OR condition — a
    // refactor that accidentally short-circuited on mesh_id alone
    // would slip past `wrong_mesh_id_rejects_*` but fail here.
    let mesh_id = MeshId::from_u128(0xC0C0_C0C0);
    let real_hash = [0x11; 32];
    let fake_hash = [0x22; 32];

    let (state, founder_id, hook_counter) = build_founder(mesh_id, real_hash);
    let addr = spawn_internal(state.clone()).await;

    let intruder = NodeId::from_u128(0xFEEDFACEFEEDFACE);
    let payload = gossip_payload(
        mesh_id,
        fake_hash, // ← mismatching hash
        vec![
            member(
                founder_id,
                "Founder",
                100,
                "127.0.0.1:9742".parse().unwrap(),
            ),
            member(
                intruder,
                "Intruder",
                200,
                "192.168.1.99:9742".parse().unwrap(),
            ),
        ],
    );

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/gossip"))
        .json(&payload)
        .send()
        .await
        .expect("internal/gossip reachable");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "wrong invite_key_hash must yield 401; got {}",
        resp.status()
    );

    let mesh = state.inner.mesh.read().await;
    assert_eq!(mesh.members.len(), 1);
    assert!(!mesh.members.contains_key(&intruder));
    drop(mesh);

    assert_eq!(hook_counter.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn matching_credentials_accept_new_member_and_fire_hook() {
    // Negative control: with matching `mesh_id` AND `invite_key_hash`,
    // the gossip merge proceeds, the new member lands in the
    // founder's view, the hook fires once. Without this, the
    // rejection assertions above might be firing on something
    // unrelated (e.g. payload parse failure) — we'd never notice.
    let mesh_id = MeshId::from_u128(0xD0D0_D0D0);
    let hash = [0x33; 32];

    let (state, founder_id, hook_counter) = build_founder(mesh_id, hash);
    let addr = spawn_internal(state.clone()).await;

    let newcomer = NodeId::from_u128(0xC0FFEE_C0FFEE);
    let payload = gossip_payload(
        mesh_id,
        hash,
        vec![
            member(
                founder_id,
                "Founder",
                100,
                "127.0.0.1:9742".parse().unwrap(),
            ),
            member(newcomer, "Newcomer", 200, "10.0.0.5:9742".parse().unwrap()),
        ],
    );

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/internal/gossip"))
        .json(&payload)
        .send()
        .await
        .expect("internal/gossip reachable");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let mesh = state.inner.mesh.read().await;
    assert_eq!(
        mesh.members.len(),
        2,
        "matching credentials must admit the newcomer"
    );
    assert!(mesh.members.contains_key(&newcomer));
    drop(mesh);

    assert_eq!(
        hook_counter.load(Ordering::Relaxed),
        1,
        "mutation hook must fire exactly once on a successful merge that added a member"
    );
}
