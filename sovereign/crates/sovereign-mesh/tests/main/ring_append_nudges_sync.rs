// SPDX-License-Identifier: AGPL-3.0-or-later
//! An act a ring app appends over HTTP reaches the peer in about a second,
//! not at the sixty-second anti-entropy tick.
//!
//! Two daemons on real sockets: A serves the REAL `client_router` (so the act
//! goes in through `POST /v1/rail/append`, the route an app actually reaches)
//! and B serves the REAL `internal_router` (so the op lands through
//! `/internal/ring/sync`). Between them sits the REAL
//! `ring_sync::spawn_ring_sync_loop` with its production sixty-second
//! interval. Nothing here is a second spelling of the path: the only knob the
//! test turns is *when* it appends.
//!
//! The interval is deliberately left at `DEFAULT_RING_SYNC_INTERVAL`. A test
//! that shortened it would pass whether or not the append nudges anything —
//! the two-second budget is only a claim about the nudge while the alternative
//! wake-up is a whole minute away.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{Person, RingRail, RingSigner, Roster};
use ed25519_dalek::SigningKey;
use sovereign_daemon::server::{client_router, internal_router};
use sovereign_daemon::state::AppState;

use crate::common;

const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const NS: &str = "ring-doc";

/// What "within about a second" is held to. Derived, not chosen: one nudged
/// round is sub-second (`ring_sync.rs`'s module header), so two seconds is
/// ample for it and nowhere near the sixty the interval alone would cost.
const BUDGET: Duration = Duration::from_secs(2);

/// One mesh both nodes appear in, with B at a real address so A's round has
/// somewhere to dial. A file roster carries the signing identity here, so no
/// member needs a pubkey — see [`node`].
fn mesh_of(a: NodeId, b: NodeId, b_addr: std::net::SocketAddr) -> Mesh {
    let mut members = HashMap::new();
    members.insert(a, common::member(a, "a", "127.0.0.1:9742".parse().unwrap()));
    members.insert(b, common::member(b, "b", b_addr));
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(7),
        name: "ring-doc nudge".into(),
        invite_key_hash: [3u8; 32],
        invite_version: 0,
        require_encryption: false,
        members,
        peers: vec![],
    }
}

/// A daemon with ring storage under `dir`, signing as `key`, on a namespace
/// whose roster file names that key.
fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: Mesh,
) -> (AppState, Arc<RingRail>) {
    // The token and the rail are construction arguments now (domains
    // REVIEW-build-appstate-*-installs, DC §4.2 "Construction is staged, and
    // parts are total"); the seed-shaped entry point is the tests' door.
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let mut members = std::collections::BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    rail.journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        self_id,
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().expect("in-memory MeshStore")),
        Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
        None,
        None,
        sovereign_daemon::state::fabric::FabricSeed {
            ring_rail: Some(Arc::clone(&rail)),
            ..Default::default()
        },
        Default::default(),
        sovereign_daemon::state::node::NodeSeed {
            client_token: Some(Arc::<str>::from(TOKEN)),
            ..Default::default()
        },
    );
    (state, rail)
}

/// How many ops `rail`'s `NS` journal holds on disk.
fn held(rail: &RingRail) -> usize {
    rail.journal(NS).unwrap().read().unwrap().0.len()
}

/// **An act appended over HTTP on A is held by B within two seconds.**
///
/// Watched RED by commenting out the `state.ring_write_nudge().notify_one()`
/// line in `sovereign-api/src/routes_rail.rs::append`: the append still
/// succeeds, A still holds its op, and B holds nothing for the whole budget
/// because the only remaining wake-up is the sixty-second tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_act_appended_over_http_is_held_by_the_peer_within_two_seconds() {
    let (ka, kb) = (
        SigningKey::from_bytes(&[1u8; 32]),
        SigningKey::from_bytes(&[2u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(1), NodeId::from_u128(2));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    // B first: A's mesh has to name B's real address.
    let (b_state, b_rail) = node(db.path(), &kb, b_id, common::solo_mesh(b_id, "b"));
    let b_addr = common::spawn_router(internal_router(b_state)).await;

    let (a_state, a_rail) = node(da.path(), &ka, a_id, mesh_of(a_id, b_id, b_addr));
    let a_addr = common::spawn_router(client_router(a_state.clone())).await;

    let _sync = sovereign_mesh::ring_sync::spawn_ring_sync_loop(
        a_state.inner.fabric.clone(),
        sovereign_mesh::ring_sync::DEFAULT_RING_SYNC_INTERVAL,
        a_state.ring_write_nudge(),
    );
    // The loop runs one round the moment it starts. Let that round finish, so
    // the only thing that can carry the act below is a nudged round — without
    // this wait the boot round could carry it and the test would pass with the
    // nudge deleted.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(held(&b_rail), 0, "control: the boot round carried nothing");

    let http = reqwest::Client::new();
    let appended = http
        .post(format!("http://{a_addr}/v1/rail/append?namespace={NS}"))
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({
            "op": "record",
            "payload": { "kind": "doc-change", "doc": NS, "update": "AQID" },
        }))
        .send()
        .await
        .unwrap();
    let at = Instant::now();
    assert_eq!(
        appended.status(),
        reqwest::StatusCode::OK,
        "{}",
        appended.text().await.unwrap()
    );
    // Asserted before B, so a failure below is about propagation and not about
    // an append that never happened.
    assert_eq!(held(&a_rail), 1, "the writer holds its own op");

    while held(&b_rail) == 0 {
        assert!(
            at.elapsed() < BUDGET,
            "B still holds nothing {}ms after the append — the append did not \
             nudge the ring round, so the act waits for the {}s tick",
            at.elapsed().as_millis(),
            sovereign_mesh::ring_sync::DEFAULT_RING_SYNC_INTERVAL.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        held(&b_rail),
        1,
        "the peer holds exactly the act that was appended"
    );
}
