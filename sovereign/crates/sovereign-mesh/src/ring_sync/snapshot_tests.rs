use super::*;
use axum::response::IntoResponse;
use commonwealth_core::ids::{MeshId, NodeId};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, Payload, Person, RailAct, RingJournal,
    RingRail, Roster, SignedOp, SigningKey,
};
use sovereign_api::server::internal_router;
use std::collections::HashMap;
use std::sync::Arc;

use super::projection_tests::*;
use super::tests::*;

/// **(c3) A snapshot that arrives in two chunks retires nothing until the
/// mark lands.** The control for (c2), and the reason the mark exists.
///
/// `exchange` pulls in chunks, so a seal can land in one and its snapshot
/// in the next — and a round can end in between (the chunk bound, a peer
/// that stops answering on call 2). At that instant the seal is on disk and
/// the live set folds EMPTY, so an unguarded reconciliation would retire
/// every row this node holds on that actor's behalf. `admit` reports the
/// journal COMPLETE there, because the hole audit runs from the floor and
/// the seal is the floor — which is why completeness cannot be the gate.
///
/// The two chunks are fed by hand rather than over the wire: what is under
/// test is the fold's verdict on a partly-arrived journal, and driving the
/// split through HTTP would make the test's own chunking the thing being
/// asserted.
///
/// Watched RED by dropping `marked` from the completeness test in
/// `rail_kv::project`: the first half retires all three of A's keys.
#[tokio::test]
async fn a_snapshot_that_arrives_in_two_chunks_retires_nothing_until_the_mark() {
    const KEYS: [&str; 3] = ["k0", "k1", "gone"];
    let (ka, kb) = (
        SigningKey::from_bytes(&[17u8; 32]),
        SigningKey::from_bytes(&[18u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(71), NodeId::from_u128(72));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);

    for k in KEYS {
        assert!(a_state
            .inner
            .fabric
            .mesh_store
            .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
            .unwrap());
    }
    assert_eq!(crate::rail_kv_pump::pump_once(&a_state).await.appended, 3);
    let a_journal = a_rail.journal(KV).unwrap();
    let b_journal = b_rail.journal(KV).unwrap();

    // B holds A's pre-seal history.
    let pre = a_journal.read().unwrap().0;
    assert_eq!(b_journal.ingest_all(&pre).unwrap(), 3);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    for k in KEYS {
        assert!(value_at(&b_state, KV, k).is_some(), "{k}");
    }

    // A deletes one key and seals.
    a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap();
    assert!(a_state.inner.fabric.mesh_store.delete(KV, "gone").unwrap());
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!((pumped.sealed, pumped.snapshot_rows), (1, 2), "{pumped:?}");
    let after = a_journal.read().unwrap().0;

    // ── Chunk one: the seal, and nothing above it.
    let (seal, rest): (Vec<_>, Vec<_>) = after
        .into_iter()
        .partition(|o| matches!(o.kind.act, RailAct::Seal));
    assert_eq!(seal.len(), 1);
    assert_eq!(b_journal.ingest_all(&seal).unwrap(), 1);
    let admitted = b_journal
        .admit(
            &crate::ring_roster::MeshRoster::from_membership(
                &*b_state.inner.fabric.mesh.read().await,
                b_state.self_node_id(),
                b_state.self_node_pubkey(),
            )
            .roster()
            .clone(),
            &Ed25519Verifier,
        )
        .unwrap();
    assert!(
        admitted.is_complete(),
        "the journal reports COMPLETE at exactly the instant its live set \
             is a lie: {:?}",
        admitted.gaps
    );
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    for k in KEYS {
        assert!(
            value_at(&b_state, KV, k).is_some(),
            "{k} was retired on the strength of half a snapshot"
        );
    }

    // ── Chunk two: the rows and the mark.
    assert_eq!(b_journal.ingest_all(&rest).unwrap(), rest.len());
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "gone"),
        None,
        "now the claim is whole"
    );
    for k in ["k0", "k1"] {
        assert!(value_at(&b_state, KV, k).is_some(), "{k}");
    }
}

/// **(d) A namespace that never leaves a machine never leaves it.**
///
/// The sender-side guard is inside the store's own transaction, so this
/// asserts on the two places a private write could surface if it slipped:
/// this node's outbox and its journals, and the peer's store. The control
/// in the same test is a public write on the same tick — without it, an
/// entirely broken pump would pass.
///
/// Watched RED by deleting the `is_gossip_excluded` guard from
/// `backend::enqueue_on`: the row queues, the pump appends it, a
/// `notes-private` journal appears on disk, and the peer refuses it at
/// `apply_projection` — the last layer, and the first three assertions all
/// go red on the way there.
#[tokio::test]
async fn an_excluded_namespace_never_enters_the_outbox_nor_a_peers_store() {
    const PRIVATE: &str = "notes-private";
    assert!(
        commonwealth_state::GOSSIP_EXCLUDED_APP_IDS.contains(&PRIVATE),
        "this test is about an excluded namespace"
    );
    let (ka, kb) = (
        SigningKey::from_bytes(&[7u8; 32]),
        SigningKey::from_bytes(&[8u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(31), NodeId::from_u128(32));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

    assert!(a_state
        .inner
        .fabric
        .mesh_store
        .set(PRIVATE, "secret", bytes::Bytes::from_static(b"mine"), a_id)
        .unwrap());
    assert!(a_state
        .inner
        .fabric
        .mesh_store
        .set(KV, "public", bytes::Bytes::from_static(b"shared"), a_id)
        .unwrap());
    assert_eq!(
        a_state.inner.fabric.mesh_store.outbox_len().unwrap(),
        1,
        "only the public write queued"
    );

    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 1, "{pumped:?}");
    let namespaces = a_rail.namespaces().unwrap();
    assert!(
        !namespaces.iter().any(|n| n == PRIVATE),
        "a private namespace has no journal at all: {namespaces:?}"
    );

    let url = serve(internal_router(b_state.clone())).await;
    let journal = a_rail.journal(KV).unwrap();
    let out = exchange(&reqwest::Client::new(), &url, &a_rail, &journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;

    assert_eq!(
        value_at(&b_state, PRIVATE, "secret"),
        None,
        "the private write is nowhere on the peer"
    );
    assert_eq!(
        value_at(&b_state, KV, "public").as_deref(),
        Some(&b"shared"[..]),
        "control: the public write on the same tick did travel"
    );
}

/// **(d2) A peer's private namespace is TAKEN by the rail and REFUSED by
/// the projection.**
///
/// (d) is the sender-side half: a private write on this node never enters
/// the outbox, so it never travels. This is the receiver-side half, and it
/// is the guard the deleted `POST /internal/app/state` route used to carry
/// in its handler — mTLS proves a caller is in the mesh, not that it runs
/// honest code, so the invariant has to survive a peer that puts a private
/// namespace on the ring deliberately. Rung 2e deleted route and handler
/// together; this is where the invariant lives now.
///
/// The rail DOES accept the ops, and the first assertion pins that rather
/// than hiding it: a namespace is a directory and `/internal/ring/sync`
/// ingests without judging an author, by design (an op's signature is
/// checked at the fold, not at the listener). So the line is on B's disk.
/// What refuses is `MeshStore::apply_projection`, and the store is the only
/// thing any reader reads. The control is a public op carried over the same
/// route in the same test — without it, a B that ingested nothing at all
/// would pass.
///
/// Watched RED by pointing `PRIVATE` at a namespace that is NOT on
/// `GOSSIP_EXCLUDED_APP_IDS` (`notes`): everything else about the test is
/// unchanged, the ops travel the same route, and the store assertion goes
/// red because the projection now takes them. That is the sabotage
/// available from this crate — the guard itself is
/// `apply_projection`'s and `commonwealth-state` watches it red by
/// deleting the arm (`apply_projection_refuses_an_excluded_namespace`).
#[tokio::test]
async fn a_peers_private_namespace_is_taken_by_the_rail_and_refused_by_the_projection() {
    const PRIVATE: &str = "notes-private";
    assert!(
        commonwealth_state::GOSSIP_EXCLUDED_APP_IDS.contains(&PRIVATE),
        "this test is about an excluded namespace"
    );
    let (ka, kb) = (
        SigningKey::from_bytes(&[13u8; 32]),
        SigningKey::from_bytes(&[14u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(51), NodeId::from_u128(52));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);
    let now = commonwealth_core::clock::unix_now_secs();

    // A is hostile: it writes the private namespace onto its own journal
    // directly, which is what a peer running patched code would do. Its own
    // store is never asked, so the outbox guard (d) pins is not in the way.
    let a_private = a_rail.journal(PRIVATE).unwrap();
    assert_eq!(
        a_private
            .ingest_all(&[kv_op(PRIVATE, &ka, 0, "secret", Some(b"mine"), now)])
            .unwrap(),
        1
    );
    let a_public = a_rail.journal(KV).unwrap();
    assert_eq!(
        a_public
            .ingest_all(&[kv_op(KV, &ka, 0, "public", Some(b"shared"), now)])
            .unwrap(),
        1
    );

    // Both namespaces go to B through the real route.
    let url = serve(internal_router(b_state.clone())).await;
    let client = reqwest::Client::new();
    for journal in [&a_private, &a_public] {
        let out = exchange(&client, &url, &a_rail, journal).await;
        assert!(out.stop.is_none(), "{:?}", out.stop);
    }

    // The rail took both — B holds the private line on disk. If this fails
    // the test below proves nothing, because nothing arrived.
    assert_eq!(
        b_rail.journal(PRIVATE).unwrap().read().unwrap().0.len(),
        1,
        "the ingest is author-blind and namespace-blind, and that is the design"
    );

    crate::rail_kv_pump::project_all_on_disk(&b_state).await;

    assert_eq!(
        value_at(&b_state, PRIVATE, "secret"),
        None,
        "a private namespace a peer pushed reached no reader"
    );
    assert_eq!(
        value_at(&b_state, KV, "public").as_deref(),
        Some(&b"shared"[..]),
        "control: an ordinary namespace over the same route in the same test did land"
    );
}

/// **(e) The ROUND is what projects, and it does so whether or not it
/// pulled anything.**
///
/// The wiring the four tests above reach past: they call
/// `project_all_on_disk` by hand, which is the boot path. In production a
/// namespace is folded once per round, after every peer — and the second
/// half of this test is why it cannot instead hang off `exchange`'s pulled
/// count. **Half the ops a node receives never pass through its own
/// `exchange`**: a peer PUSHES on call 2 of its exchange, and those land
/// through `/internal/ring/sync`, a route in another crate. A projection
/// conditioned on our own pull would be blind to exactly the direction the
/// pump's nudge creates.
///
/// Watched RED by moving the projection inside `if ex.pulled > 0`: the
/// first half still passes and the converged round below reports
/// `namespaces_projected: 0`, which is the shape of the bug.
#[tokio::test]
async fn a_round_projects_the_namespace_even_when_it_pulled_nothing() {
    let (ka, kb) = (
        SigningKey::from_bytes(&[9u8; 32]),
        SigningKey::from_bytes(&[10u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(41), NodeId::from_u128(42));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let now = commonwealth_core::clock::unix_now_secs();

    let a_dir = da.path().to_path_buf();
    let mut mesh_for_b = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(&a_dir, &ka, a_id, kv_mesh(&ka, &kb, a_id, b_id));
    a_rail
        .journal(KV)
        .unwrap()
        .ingest_all(&[kv_op(KV, &ka, 0, "from-a", Some(b"a"), now)])
        .unwrap();
    let a_addr = serve_at(internal_router(a_state.clone())).await;

    // B's view of the mesh has A at a real address; everything else about
    // the two states is identical.
    if let Some(record) = mesh_for_b.members.get_mut(&a_id) {
        record.addresses = vec![a_addr];
    }
    let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh_for_b);
    b_rail
        .journal(KV)
        .unwrap()
        .ingest_all(&[kv_op(KV, &kb, 0, "from-b", Some(b"b"), now)])
        .unwrap();

    let round = run_one_round(&b_state).await;
    assert_eq!(round.peers_reached, 1, "{round:?}");
    assert_eq!((round.ops_pulled, round.ops_pushed), (1, 1), "{round:?}");
    assert_eq!(round.namespaces_projected, 1, "{round:?}");
    assert_eq!(
        value_at(&b_state, KV, "from-a").as_deref(),
        Some(&b"a"[..]),
        "the round folded what it pulled"
    );

    // Converged: nothing moves, and the namespace is projected anyway.
    let round = run_one_round(&b_state).await;
    assert_eq!((round.ops_pulled, round.ops_pushed), (0, 0), "{round:?}");
    assert_eq!(
        round.namespaces_projected, 1,
        "a round that pulled nothing still folds — ops a peer PUSHED \
             arrive by a route this loop never runs: {round:?}"
    );
}

/// **(f) Retention on a rail-backed namespace stays retained.**
///
/// RED-FIRST, and the direction is the whole point: the store is a
/// PROJECTION now, so a row a local sweep deletes has no incumbent and the
/// next round's `merge_entry` puts it straight back from the journal. The
/// sweep runs on a 60s-ish cadence and the fold runs on a 60s round, so a
/// 30-day retention window on a meshed node is undone within a minute,
/// every minute, forever.
#[tokio::test]
async fn a_retention_sweep_is_not_undone_by_the_next_projection() {
    const LEDGER: &str = commonwealth_state::CONTRIBUTIONS_APP_ID;
    let (ka, kb) = (
        SigningKey::from_bytes(&[15u8; 32]),
        SigningKey::from_bytes(&[16u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(61), NodeId::from_u128(62));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (_a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

    let now = commonwealth_core::clock::unix_now_secs();
    let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
    let floor = now - window;
    // Two ledger events A signed: one a day past the aggregation window,
    // one inside it. Days apart from the boundary, so no clock tick
    // between the plant and the sweep can move which side either is on.
    let a_journal = a_rail.journal(LEDGER).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[
                kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
            ])
            .unwrap(),
        2
    );

    let url = serve(internal_router(b_state.clone())).await;
    let out = exchange(&reqwest::Client::new(), &url, &a_rail, &a_journal).await;
    assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert!(
        value_at(&b_state, LEDGER, "fresh-event").is_some(),
        "control: the ledger replicated at all"
    );

    // B's retention sweep, at the ONE cutoff the namespace's readers use.
    b_state
        .inner
        .fabric
        .mesh_store
        .gc_app_before(LEDGER, floor)
        .unwrap();
    assert_eq!(
        value_at(&b_state, LEDGER, "old-event"),
        None,
        "control: the sweep did delete the row"
    );

    // …and now one more round of the very thing that fills the store.
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, LEDGER, "old-event"),
        None,
        "the projection put back a row retention had just taken"
    );
    assert!(
        value_at(&b_state, LEDGER, "fresh-event").is_some(),
        "and it took only the expired one"
    );
}

/// **(f2) The author's own sweep is not undone by the author's own
/// journal, and it puts nothing on the rail.**
///
/// The other half of (f). A's store leads its journal by an outbox drain,
/// so `apply_projection` deliberately does not reconcile A's own rows
/// against A's sealed live set — which means the ONLY thing that can keep
/// A's expired rows out of A's store is the fold itself refusing them.
///
/// The outbox assertion is the second claim: expiry emits no traffic. Every
/// node derives the same floor from the same `t`, so retention needs no
/// message — and a tombstone per retired row would grow the journal
/// retention exists to bound.
#[tokio::test]
async fn an_authors_own_retention_sweep_is_not_undone_and_puts_nothing_on_the_rail() {
    const LEDGER: &str = commonwealth_state::CONTRIBUTIONS_APP_ID;
    let ka = SigningKey::from_bytes(&[17u8; 32]);
    let kb = SigningKey::from_bytes(&[18u8; 32]);
    let (a_id, b_id) = (NodeId::from_u128(71), NodeId::from_u128(72));
    let da = tempfile::tempdir().unwrap();
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, kv_mesh(&ka, &kb, a_id, b_id));

    let now = commonwealth_core::clock::unix_now_secs();
    let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
    let floor = now - window;
    let a_journal = a_rail.journal(LEDGER).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[
                kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
            ])
            .unwrap(),
        2
    );
    crate::rail_kv_pump::project_all_on_disk(&a_state).await;
    assert!(
        value_at(&a_state, LEDGER, "fresh-event").is_some(),
        "control: A folded its own journal"
    );

    a_state
        .inner
        .fabric
        .mesh_store
        .gc_app_before(LEDGER, floor)
        .unwrap();
    assert_eq!(
        a_state.inner.fabric.mesh_store.outbox_len().unwrap(),
        0,
        "an expiry is not a delete: it publishes nothing, because every \
             node derives the same floor from the same `t`"
    );

    crate::rail_kv_pump::project_all_on_disk(&a_state).await;
    assert_eq!(
        value_at(&a_state, LEDGER, "old-event"),
        None,
        "A's own journal put back a row A's own retention had just taken"
    );
    assert!(
        value_at(&a_state, LEDGER, "fresh-event").is_some(),
        "and it took only the expired one"
    );
}
