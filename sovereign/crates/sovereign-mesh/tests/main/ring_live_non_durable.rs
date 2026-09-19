// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The live lane lands in no store, and a restart is the whole of its
//! retention policy.**
//!
//! The order's day-3 step names two things watched failing: the
//! replication-sender census stays green (its own file — it proves the lane
//! is not declared as replicated state) and a restart empties the buffer.
//! The census can only see a second URL-join site on a route it already
//! names, so it can never redden for a handler that writes to a store
//! (`replication_sender_census.rs:131-139`). This file is the half that can:
//! it snapshots every byte under the rail directory across a live payload.
//!
//! One daemon, real sockets, the REAL routers: payloads go in through
//! `internal_router`'s `/internal/ring/live` (the route a peer reaches) and
//! come out through `client_router`'s `GET /v1/rail/live` (the route the page
//! reaches). No ring-sync loop is spawned, deliberately — the snapshot below
//! is of a directory nothing else is allowed to touch, so a diff in it is
//! this lane's or nobody's.

use std::collections::BTreeMap;
use std::sync::Arc;

use commonwealth_core::ids::NodeId;
use commonwealth_rail::{Person, RingRail, RingSigner, Roster};
use ed25519_dalek::SigningKey;
use sovereign_daemon::server::{client_router, client_router_for, internal_router, ClientSurface};
use sovereign_daemon::state::AppState;
use sovereign_grants::Scope;

use crate::common;

const TOKEN: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";
const NS: &str = "ring-doc";
/// A second namespace with its own grant, for the isolation test.
const NS_B: &str = "tool-lending";
const GRANT_A: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
const GRANT_B: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

/// The wire shape a peer's `push_ephemeral` posts to `/internal/ring/live`.
fn envelope(namespace: &str, payload: &str) -> String {
    serde_json::json!({ "namespace": namespace, "payload": payload }).to_string()
}

/// A daemon with ring storage under `dir`, signing as `key`, on a namespace
/// whose roster file names that key — the same node shape
/// `ring_append_nudges_sync.rs` builds, so the rail here is the production
/// one and not a stub that could not write even if the handler asked it to.
fn node(dir: &std::path::Path, key: &SigningKey, self_id: NodeId) -> AppState {
    // The token and the rail are construction arguments now (domains
    // REVIEW-build-appstate-*-installs); the seed-shaped entry point is the
    // tests' door.
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let mut members = BTreeMap::new();
    members.insert(Person::from("alex"), vec![key.actor()]);
    rail.journal(NS)
        .unwrap()
        .set_roster(&Roster::new(members))
        .unwrap();
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric_and_serving_and_node(
        self_id,
        common::solo_mesh(self_id, "a"),
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
        },
    );
    // `/internal/ring/live` refuses a namespace no live rail grant names.
    grant(&state, GRANT_A, NS);
    state
}

fn grant(state: &AppState, token: &str, namespace: &str) {
    let now = commonwealth_core::clock::unix_now_millis();
    state.inner.node.guest_grants.issue(
        token,
        vec![Scope::Rails(namespace.into())],
        Some("ring app".into()),
        3_600,
        now,
    );
}

/// Every file under `dir`, by relative path, with its length.
///
/// Length rather than content hash on purpose: the assertion has to name
/// WHICH file grew when it fails, and a map of sizes prints that where a
/// single digest over the tree would print only "something changed".
fn snapshot(dir: &std::path::Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let rel = entry
                    .path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, meta.len());
            }
        }
    }
    out
}

/// **A live payload changes no byte under the rail directory — and arrives.**
///
/// Both halves are load-bearing. Without the drain assertion the test passes
/// on a daemon that throws every payload away, which is not the claim: the
/// claim is that the lane DELIVERS and does not RECORD.
///
/// Watched RED by making `/internal/ring/live` also append the payload as an
/// act to `state.ring_rail()`'s journal: the drain still returns all three,
/// and the snapshot names the journal file that grew.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_payload_touches_nothing_on_disk() {
    let key = SigningKey::from_bytes(&[11u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let state = node(dir.path(), &key, NodeId::from_u128(1));

    let internal = common::spawn_router(internal_router(state.clone())).await;
    let client = common::spawn_router(client_router(state.clone())).await;

    // Taken AFTER the roster write, so the roster's own bytes are part of the
    // baseline rather than showing up as the change this test is hunting.
    let before = snapshot(dir.path());
    assert!(
        !before.is_empty(),
        "control: the rail directory holds the roster this node just wrote — \
         an empty baseline would make the comparison below vacuous"
    );

    let http = reqwest::Client::new();
    let payloads = ["AQID", "BAUG", "BwgJ"];
    for payload in payloads {
        let sent = http
            .post(format!("http://{internal}/internal/ring/live"))
            .body(envelope(NS, payload))
            .send()
            .await
            .unwrap();
        assert_eq!(
            sent.status(),
            reqwest::StatusCode::OK,
            "{}",
            sent.text().await.unwrap()
        );
    }

    // The vacuity guard: the lane carried all three.
    let drained: serde_json::Value = http
        .get(format!("http://{client}/v1/rail/live?namespace={NS}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        drained["payloads"],
        serde_json::json!(payloads),
        "the drain returns exactly what the peer pushed, in order"
    );
    assert_eq!(
        drained["dropped"], 0,
        "nothing was evicted at three payloads"
    );

    assert_eq!(
        snapshot(dir.path()),
        before,
        "a live payload changed bytes under the rail directory — the lane \
         recorded something, and the campaign's predicate is that it cannot"
    );
}

/// **A restart empties the buffer.**
///
/// A second `AppState` over the SAME directory — the shape a daemon restart
/// has — drains empty. The first one has the payload in hand, so the test
/// cannot pass by never having buffered anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_empties_the_buffer() {
    let key = SigningKey::from_bytes(&[12u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let http = reqwest::Client::new();

    let first = node(dir.path(), &key, NodeId::from_u128(1));
    let first_internal = common::spawn_router(internal_router(first.clone())).await;
    let first_client = common::spawn_router(client_router(first.clone())).await;
    http.post(format!("http://{first_internal}/internal/ring/live"))
        .body(envelope(NS, "AQID"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    // Control: before the restart the payload IS there.
    let live: serde_json::Value = http
        .get(format!("http://{first_client}/v1/rail/live?namespace={NS}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        live["payloads"],
        serde_json::json!(["AQID"]),
        "control: the buffer held the payload while the daemon was up"
    );

    let second = node(dir.path(), &key, NodeId::from_u128(1));
    let second_client = common::spawn_router(client_router(second)).await;
    let after: serde_json::Value = http
        .get(format!(
            "http://{second_client}/v1/rail/live?namespace={NS}"
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["payloads"],
        serde_json::json!([]),
        "a daemon over the same directory drained a live payload it never \
         received — the lane persisted something"
    );
}

/// Drain `/v1/rail/live` as the holder of `token`, on the Rail surface — the
/// listener a ring app reaches, where loopback is NOT trusted, so the grant is
/// what resolves the namespace.
async fn drain_as(rail: std::net::SocketAddr, token: &str) -> serde_json::Value {
    let resp = reqwest::Client::new()
        .get(format!("http://{rail}/v1/rail/live"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.unwrap()
}

/// **A grant drains only its own namespace.**
///
/// Two grants for two namespaces on one daemon; three payloads arrive for the
/// first. The second grant's drain comes back empty, and the first's returns
/// all three — so the empty drain is isolation, not a lane that lost them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grant_drains_only_its_own_namespace() {
    let key = SigningKey::from_bytes(&[13u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let state = node(dir.path(), &key, NodeId::from_u128(1));
    grant(&state, GRANT_B, NS_B);

    let internal = common::spawn_router(internal_router(state.clone())).await;
    let rail = common::spawn_router(client_router_for(state.clone(), ClientSurface::Rail)).await;

    let http = reqwest::Client::new();
    let payloads = ["AQID", "BAUG", "BwgJ"];
    for payload in payloads {
        http.post(format!("http://{internal}/internal/ring/live"))
            .body(envelope(NS, payload))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    let other = drain_as(rail, GRANT_B).await;
    assert_eq!(
        other["payloads"],
        serde_json::json!([]),
        "a grant for '{NS_B}' drained payloads pushed under '{NS}'"
    );
    let own = drain_as(rail, GRANT_A).await;
    assert_eq!(
        own["payloads"],
        serde_json::json!(payloads),
        "the grant for '{NS}' drains exactly what arrived under it, in order"
    );
}

/// **An envelope for a namespace no grant names is refused, and buffers
/// nothing.**
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_envelope_for_an_unknown_namespace_is_refused() {
    let key = SigningKey::from_bytes(&[14u8; 32]);
    let dir = tempfile::tempdir().unwrap();
    let state = node(dir.path(), &key, NodeId::from_u128(1));

    let internal = common::spawn_router(internal_router(state.clone())).await;
    let client = common::spawn_router(client_router(state.clone())).await;
    let rail = common::spawn_router(client_router_for(state.clone(), ClientSurface::Rail)).await;

    let http = reqwest::Client::new();
    let unknown = "nobody-granted-this";
    let resp = http
        .post(format!("http://{internal}/internal/ring/live"))
        .body(envelope(unknown, "AQID"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    assert!(
        !status.is_success(),
        "an envelope for an ungranted namespace was accepted: {status} {body}"
    );
    assert!(
        body.contains(unknown),
        "the refusal must name the namespace it refused: {body}"
    );

    let own = drain_as(rail, GRANT_A).await;
    assert_eq!(
        own["payloads"],
        serde_json::json!([]),
        "the granted buffer is untouched"
    );
    let refused: serde_json::Value = http
        .get(format!("http://{client}/v1/rail/live?namespace={unknown}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        refused["payloads"],
        serde_json::json!([]),
        "the refused payload was buffered under its namespace anyway"
    );
}
