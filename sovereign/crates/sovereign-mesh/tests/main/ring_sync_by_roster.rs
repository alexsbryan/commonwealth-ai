// SPDX-License-Identifier: AGPL-3.0-or-later
//! A ring is read by its roster — offered to it, and served to it.
//!
//! The failing input is the shape `mp-2` was minted against: `run_one_round`
//! built ONE peer list per round out of every Online member and read no
//! roster, and `/internal/ring/sync` extracted `State` and `Bytes` and so
//! could not name its asker even to log it. A mesh member on no ring's roster
//! was sent every ring this node held, and could have asked for any of them by
//! name.
//!
//! Three daemons, because two cannot express the case: A writes, B is on the
//! roster, C is a full mesh member and is not. Every clause of
//! `mp-ring-reads-by-roster` is one test here, and the two halves are
//! deliberately separable — (a) is the SENDER's filter and (b) is the
//! SERVER's, which is why dropping the server's check leaves (a) green. That
//! asymmetry is the bar's own goodhart and the plant that proves it.
//!
//! The requests `run_one_round` makes here carry no acceptor stamp — this is a
//! plaintext, in-process mesh — so the serving side sees `Anonymous` and
//! serves. That is the disclosed gap named in `routes_internal::ring_sync`'s
//! module header, and it is why (b) presents a stamped identity the way the
//! iroh acceptor does rather than relying on the round to carry one.

use std::collections::HashMap;
use std::sync::Arc;

use commonwealth_core::ids::{MeshId, NodeId, NodePubkey};
use commonwealth_core::mesh::{MemberRecord, Mesh};
use commonwealth_rail::{Person, RailAct, RingRail, RingSigner, Roster};
use ed25519_dalek::SigningKey;
use sovereign_daemon::server::internal_router;
use sovereign_daemon::state::AppState;

use crate::common;

/// The narrowed ring: a `roster.json` naming two of the three members.
const NS_FILE: &str = "house-expenses";
/// A ring nobody narrowed. No `roster.json`, so the rail's DEFAULT roster —
/// mesh membership — answers it, and every member with a key is on it.
const NS_OPEN: &str = "house-photos";
/// A REGISTERED namespace: membership answers it and no file may narrow it
/// (`ring_roster::REGISTERED_NAMESPACES`).
const NS_REGISTERED: &str = sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID;

const MESH_ID: u128 = 71;
const INVITE_HASH: [u8; 32] = [29u8; 32];

/// A member row that carries its verified key, which is what a real join
/// gossips and what every derived roster is built from. `common::member`
/// leaves `node_pubkey` absent — a pre-identity build — and such a member is
/// on no roster at all, derived or hand-written.
fn keyed_member(id: NodeId, name: &str, key: &SigningKey, addr: &str) -> MemberRecord {
    let mut rec = common::member(id, name, addr.parse().unwrap());
    rec.node_pubkey = Some(NodePubkey(key.verifying_key().to_bytes()));
    rec
}

fn mesh_with(members: Vec<MemberRecord>) -> Mesh {
    let mut map = HashMap::new();
    for m in members {
        map.insert(m.node_id, m);
    }
    Mesh {
        mesh_secret: [0u8; 32],
        invite_expires_at: None,
        id: MeshId::from_u128(MESH_ID),
        name: "three".into(),
        invite_key_hash: INVITE_HASH,
        invite_version: 0,
        require_encryption: false,
        members: map,
        peers: vec![],
    }
}

/// The roster file `NS_FILE` carries on a node: one person per key named.
fn file_roster(named: &[(&str, &SigningKey)]) -> Roster {
    let mut members = std::collections::BTreeMap::new();
    for (person, key) in named {
        members.insert(Person::from(*person), vec![key.actor()]);
    }
    Roster::new(members)
}

/// A daemon with ring storage under `dir`, signing as `key`, with membership
/// installed as the rail's derived-roster source exactly as the daemon does.
///
/// `NS_FILE` gets `rostered` written to its `roster.json`; `NS_OPEN` and
/// `NS_REGISTERED` get no file, so they are answered by membership.
fn node(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: Mesh,
    rostered: &[(&str, &SigningKey)],
) -> (AppState, Arc<RingRail>) {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    rail.journal(NS_FILE)
        .unwrap()
        .set_roster(&file_roster(rostered))
        .unwrap();
    // Opened so the namespace exists on disk: `RingRail::namespaces` reads the
    // directory, and a round can only offer a ring this node holds.
    rail.journal(NS_OPEN).unwrap();
    rail.journal(NS_REGISTERED).unwrap();
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
        Default::default(),
    );
    sovereign_mesh::ring_roster::MeshRosterSource::install(
        &rail,
        &state.inner.fabric.mesh,
        &state.inner.fabric.identity,
        Some(NodePubkey(key.verifying_key().to_bytes())),
    )
    .unwrap();
    (state, rail)
}

/// Sign and append one act onto `ns`, through the rail's own door.
async fn append(rail: &RingRail, key: &SigningKey, ns: &str, amount: u64) {
    let journal = rail.journal(ns).unwrap();
    let roster = rail.roster(&journal).await.expect("a readable roster");
    journal
        .append(
            RailAct::Record {
                payload: commonwealth_rail::Payload::new(
                    serde_json::json!({ "kind": "expense", "amount": amount }),
                )
                .expect("a well-formed record payload"),
            },
            &key.clone(),
            &roster,
            None,
        )
        .unwrap_or_else(|e| panic!("append to {ns}: {e}"));
}

/// The ops `rail` holds on `ns`, in the journal's own order.
fn ops(rail: &RingRail, ns: &str) -> Vec<commonwealth_rail::Op<commonwealth_rail::SignedOp>> {
    rail.journal(ns).unwrap().read().unwrap().0
}

fn held(rail: &RingRail, ns: &str) -> usize {
    ops(rail, ns).len()
}

/// The three keys and node ids this file's scenarios share.
struct Cast {
    ka: SigningKey,
    kb: SigningKey,
    kc: SigningKey,
    a: NodeId,
    b: NodeId,
    c: NodeId,
}

fn cast() -> Cast {
    Cast {
        ka: SigningKey::from_bytes(&[21u8; 32]),
        kb: SigningKey::from_bytes(&[22u8; 32]),
        kc: SigningKey::from_bytes(&[23u8; 32]),
        a: NodeId::from_u128(401),
        b: NodeId::from_u128(402),
        c: NodeId::from_u128(403),
    }
}

/// A, B and C, with B and C's internal routers live and A's membership naming
/// their real addresses. `a_rostered` is what A's `NS_FILE` roster names.
async fn three_nodes(
    cast: &Cast,
    dirs: &(tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
    a_rostered: &[(&str, &SigningKey)],
) -> ((AppState, Arc<RingRail>), Arc<RingRail>, Arc<RingRail>) {
    let both = [("alex", &cast.ka), ("bea", &cast.kb)];
    let (b_state, b_rail) = node(
        dirs.1.path(),
        &cast.kb,
        cast.b,
        mesh_with(vec![keyed_member(cast.b, "bea", &cast.kb, "127.0.0.1:2")]),
        &both,
    );
    let b_addr = common::spawn_router(internal_router(b_state)).await;
    let (c_state, c_rail) = node(
        dirs.2.path(),
        &cast.kc,
        cast.c,
        mesh_with(vec![keyed_member(cast.c, "cass", &cast.kc, "127.0.0.1:3")]),
        &both,
    );
    let c_addr = common::spawn_router(internal_router(c_state)).await;

    let (a_state, a_rail) = node(
        dirs.0.path(),
        &cast.ka,
        cast.a,
        mesh_with(vec![
            keyed_member(cast.a, "alex", &cast.ka, "127.0.0.1:1"),
            keyed_member(cast.b, "bea", &cast.kb, &b_addr.to_string()),
            keyed_member(cast.c, "cass", &cast.kc, &c_addr.to_string()),
        ]),
        a_rostered,
    );
    ((a_state, a_rail), b_rail, c_rail)
}

/// **(a) A ring is offered to its roster and to nobody else.**
///
/// Watched RED by reverting `run_one_round`'s per-namespace filter to the
/// round-level peer list: C is a mesh member, so it received the whole
/// `house-expenses` journal it is on no roster for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ring_is_offered_to_its_roster_and_to_no_other_member() {
    let cast = cast();
    let dirs = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let ((a_state, a_rail), b_rail, c_rail) =
        three_nodes(&cast, &dirs, &[("alex", &cast.ka), ("bea", &cast.kb)]).await;

    append(&a_rail, &cast.ka, NS_FILE, 1).await;
    append(&a_rail, &cast.ka, NS_FILE, 2).await;
    assert_eq!(held(&a_rail, NS_FILE), 2, "the writer holds its own ops");

    // Two rounds: one is enough to converge, and a second proves the filter is
    // not a first-round accident.
    for _ in 0..2 {
        sovereign_mesh::ring_sync::run_one_round(&a_state.inner.fabric).await;
    }

    assert_eq!(
        ops(&b_rail, NS_FILE),
        ops(&a_rail, NS_FILE),
        "the roster member converges byte-equal with the writer"
    );
    assert_eq!(
        held(&c_rail, NS_FILE),
        0,
        "C is a full mesh member and is on no roster for {NS_FILE} — it must \
         hold nothing, and the round must never have offered it"
    );
}

/// **(c) A ring nobody narrowed still reaches every member.**
///
/// Both kinds of derived roster: one with no `roster.json` at all, answered by
/// the rail's DEFAULT source, and one on `REGISTERED_NAMESPACES`, which no
/// file may narrow. This is the clause the filter could break invisibly —
/// every daemon-written ring would quietly stop replicating.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_derived_roster_still_reaches_every_member() {
    let cast = cast();
    let dirs = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let ((a_state, a_rail), b_rail, c_rail) =
        three_nodes(&cast, &dirs, &[("alex", &cast.ka), ("bea", &cast.kb)]).await;

    for ns in [NS_OPEN, NS_REGISTERED] {
        assert_eq!(
            a_state
                .inner
                .fabric
                .ring_rail()
                .expect("rail installed")
                .roster_origin(ns),
            commonwealth_rail::RosterOrigin::Derived,
            "{ns} must be answered by membership or this test is not the case \
             it names"
        );
        append(&a_rail, &cast.ka, ns, 7).await;
    }

    sovereign_mesh::ring_sync::run_one_round(&a_state.inner.fabric).await;

    for ns in [NS_OPEN, NS_REGISTERED] {
        assert_eq!(held(&b_rail, ns), 1, "{ns} must reach B");
        assert_eq!(
            held(&c_rail, ns),
            1,
            "{ns}'s roster is derived from membership, which names C — the \
             filter must not narrow it"
        );
    }
}

/// **(b) The serving side refuses a namespace whose roster does not name the
/// asker, by name.**
///
/// C presents what this node's OWN iroh acceptor presents — the verified
/// triple plus the per-process mark — so the request resolves to
/// `Principal::Member { c }` exactly as a real dial would. Without that it
/// would be `Anonymous` and this test would be about a different caller.
///
/// Watched RED by deleting the `roster_refusal` call from the handler: the
/// route answers 200 with a digest and one budget of the journal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sync_route_refuses_a_member_the_roster_does_not_name() {
    let cast = cast();
    let da = tempfile::tempdir().unwrap();
    let a_mesh = mesh_with(vec![
        keyed_member(cast.a, "alex", &cast.ka, "127.0.0.1:1"),
        keyed_member(cast.b, "bea", &cast.kb, "127.0.0.1:2"),
        keyed_member(cast.c, "cass", &cast.kc, "127.0.0.1:3"),
    ]);
    let (a_state, a_rail) = node(
        da.path(),
        &cast.ka,
        cast.a,
        a_mesh,
        &[("alex", &cast.ka), ("bea", &cast.kb)],
    );
    append(&a_rail, &cast.ka, NS_FILE, 3).await;
    let a_addr = common::spawn_router(internal_router(a_state)).await;

    let ask = |ns: &str, id: NodeId, key: &SigningKey, name: &str| {
        let body = serde_json::json!({
            "namespace": ns,
            "digest": commonwealth_rail::Digest::default(),
            "ops": [],
        });
        let req = reqwest::Client::new()
            .post(format!("http://{a_addr}/internal/ring/sync"))
            .json(&body);
        common::acceptor_stamp(req, name, id, key.verifying_key().to_bytes()).send()
    };

    let refused = ask(NS_FILE, cast.c, &cast.kc, "cass").await.unwrap();
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a verified member the roster does not name must be refused"
    );
    let said = refused.text().await.unwrap();
    assert!(
        said.contains(NS_FILE) && said.contains(&cast.c.to_string()),
        "the refusal must name the namespace and the asker; it said: {said}"
    );

    // The control, and the half that makes the refusal mean something: the
    // same request from the member the roster DOES name is served.
    let served = ask(NS_FILE, cast.b, &cast.kb, "bea").await.unwrap();
    assert_eq!(
        served.status(),
        reqwest::StatusCode::OK,
        "the roster member must still be served: {}",
        served.text().await.unwrap()
    );

    // And the derived namespace is served to C, because membership names it.
    let open = ask(NS_OPEN, cast.c, &cast.kc, "cass").await.unwrap();
    assert_eq!(
        open.status(),
        reqwest::StatusCode::OK,
        "{NS_OPEN} is answered by membership, which names C"
    );
}

/// **(d) A member added to the roster holds the whole journal one round later,
/// with no restart.**
///
/// The roster is a parameter of every round rather than something read at
/// boot, so adding a key is the whole operation — and what the new member
/// receives is the journal entire, not just what was written after it joined.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_added_to_the_roster_receives_the_whole_journal_next_round() {
    let cast = cast();
    let dirs = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let ((a_state, a_rail), _b_rail, c_rail) =
        three_nodes(&cast, &dirs, &[("alex", &cast.ka), ("bea", &cast.kb)]).await;

    append(&a_rail, &cast.ka, NS_FILE, 4).await;
    append(&a_rail, &cast.ka, NS_FILE, 5).await;
    sovereign_mesh::ring_sync::run_one_round(&a_state.inner.fabric).await;
    assert_eq!(
        held(&c_rail, NS_FILE),
        0,
        "control: C starts off the roster"
    );

    // The one operation: C's key joins the roster file. Nothing restarts.
    a_rail
        .journal(NS_FILE)
        .unwrap()
        .set_roster(&file_roster(&[
            ("alex", &cast.ka),
            ("bea", &cast.kb),
            ("cass", &cast.kc),
        ]))
        .unwrap();

    sovereign_mesh::ring_sync::run_one_round(&a_state.inner.fabric).await;

    assert_eq!(
        ops(&c_rail, NS_FILE),
        ops(&a_rail, NS_FILE),
        "the newly rostered member holds the journal entire, including the ops \
         written before it was on the roster"
    );
}
