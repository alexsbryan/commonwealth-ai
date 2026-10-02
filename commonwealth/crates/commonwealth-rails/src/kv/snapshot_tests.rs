// SPDX-License-Identifier: AGPL-3.0-or-later
//! Snapshot, privacy and retention, two nodes apart — moved from
//! sovereign-daemon's `ring_sync_snapshot_tests` in five-programs fp-83 with
//! their assertions verbatim, over the fixtures in `projection_tests`.
//!
//! `a_round_projects_the_namespace_even_when_it_pulled_nothing` did not move:
//! it pinned the daemon ring round's own projection step, which fp-83 deleted
//! with the store it folded into. Its claim — ops a peer PUSHES reach the
//! store although this node's own exchange never pulled them — is pinned here
//! by `a_peer_op_ingested_through_the_door_is_served_after_one_fold` (fp-109):
//! every op arrives through the ingest door, and the door marks the fold.

use commonwealth_core::ids::NodeId;
use commonwealth_rail::{Ed25519Verifier, RailAct, SigningKey};

use super::projection_tests::{exchange, filler, ingest, kv_mesh, kv_node, kv_op, value_at, KV};

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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    for k in KEYS {
        assert!(a
            .store
            .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
            .unwrap());
    }
    assert_eq!(a.pump_once().await.appended, 3);
    let a_journal = a.rail.journal(KV).unwrap();
    let b_journal = b.rail.journal(KV).unwrap();

    // B holds A's pre-seal history.
    let pre = a_journal.read().unwrap().0;
    assert_eq!(b_journal.ingest_all(&pre).unwrap(), 3);
    b.project_all_on_disk().await;
    for k in KEYS {
        assert!(value_at(&b, KV, k).is_some(), "{k}");
    }

    // A deletes one key and seals.
    a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap();
    assert!(a.store.delete(KV, "gone").unwrap());
    let pumped = a.pump_once().await;
    assert_eq!((pumped.sealed, pumped.snapshot_rows), (1, 2), "{pumped:?}");
    let after = a_journal.read().unwrap().0;

    // ── Chunk one: the seal, and nothing above it.
    let (seal, rest): (Vec<_>, Vec<_>) = after
        .into_iter()
        .partition(|o| matches!(o.kind.act, RailAct::Seal));
    assert_eq!(seal.len(), 1);
    assert_eq!(b_journal.ingest_all(&seal).unwrap(), 1);
    let admitted = b_journal
        .admit(&b.rail.roster(&b_journal).await.unwrap(), &Ed25519Verifier)
        .unwrap();
    assert!(
        admitted.is_complete(),
        "the journal reports COMPLETE at exactly the instant its live set \
             is a lie: {:?}",
        admitted.gaps
    );
    b.project_all_on_disk().await;
    for k in KEYS {
        assert!(
            value_at(&b, KV, k).is_some(),
            "{k} was retired on the strength of half a snapshot"
        );
    }

    // ── Chunk two: the rows and the mark.
    assert_eq!(b_journal.ingest_all(&rest).unwrap(), rest.len());
    b.project_all_on_disk().await;
    assert_eq!(value_at(&b, KV, "gone"), None, "now the claim is whole");
    for k in ["k0", "k1"] {
        assert!(value_at(&b, KV, k).is_some(), "{k}");
    }
}

/// **(d) A namespace that never leaves a machine never leaves it.**
///
/// Since fp-107 a local-only write IS queued and the pump journals it (fp-76's
/// class: journaled, never offered), so this asserts on the two places a
/// private write could surface on the wire: the namespaces the ring offers,
/// and the peer's store. The control in the same test is a public write on
/// the same tick — without it, an entirely broken pump would pass.
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    assert!(a
        .store
        .set(PRIVATE, "secret", bytes::Bytes::from_static(b"mine"), a_id)
        .unwrap());
    assert!(a
        .store
        .set(KV, "public", bytes::Bytes::from_static(b"shared"), a_id)
        .unwrap());
    assert_eq!(
        a.store.outbox_len().unwrap(),
        2,
        "both writes queued: the private one to be journaled, never offered"
    );

    let pumped = a.pump_once().await;
    assert_eq!(pumped.appended, 2, "{pumped:?}");
    let namespaces = a.rail.namespaces().unwrap();
    assert!(
        !namespaces.iter().any(|n| n == PRIVATE),
        "a private journal is never offered: {namespaces:?}"
    );

    exchange(&a, &b, KV).await;
    b.project_all_on_disk().await;

    assert_eq!(
        value_at(&b, PRIVATE, "secret"),
        None,
        "the private write is nowhere on the peer"
    );
    assert_eq!(
        value_at(&b, KV, "public").as_deref(),
        Some(&b"shared"[..]),
        "control: the public write on the same tick did travel"
    );
}

/// **(d2) A peer's private namespace is TAKEN by the rail and REFUSED by
/// the fold.**
///
/// (d) is the sender-side half. This is the receiver-side half: the
/// invariant has to survive a peer that puts a private namespace on the ring
/// deliberately, because mesh membership proves a caller is in the mesh, not
/// that it runs honest code.
///
/// The rail DOES accept the ops, and the first assertion pins that rather
/// than hiding it: a namespace is a directory and the ingest door takes ops
/// without judging an author, by design (an op's signature is checked at the
/// fold, not at the door). So the line is on B's disk. What keeps it from a
/// reader is the fold: `RingRail::namespaces` never lists a local-only
/// journal, the store namespace fold skips an excluded one, and the
/// local-only rehydrate takes this node's own actor only (fp-108). The
/// control is a public op carried the same way in the same test — without
/// it, a B that ingested nothing at all would pass.
///
/// Watched RED by pointing `PRIVATE` at a namespace that is NOT on
/// `GOSSIP_EXCLUDED_APP_IDS` (`notes`): everything else about the test is
/// unchanged, the ops travel the same door, and the store assertion goes
/// red because the fold now takes them.
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);
    let now = commonwealth_core::clock::unix_now_secs();

    // A is hostile: it writes the private namespace onto its own journal
    // directly, which is what a peer running patched code would do. Its own
    // store is never asked, so the outbox guard (d) pins is not in the way.
    let a_private = a.rail.journal(PRIVATE).unwrap();
    assert_eq!(
        a_private
            .ingest_all(&[kv_op(PRIVATE, &ka, 0, "secret", Some(b"mine"), now)])
            .unwrap(),
        1
    );
    let a_public = a.rail.journal(KV).unwrap();
    assert_eq!(
        a_public
            .ingest_all(&[kv_op(KV, &ka, 0, "public", Some(b"shared"), now)])
            .unwrap(),
        1
    );

    // Both namespaces go to B through the ingest door. The private one is a
    // raw push: an honest exchange offers a local-only journal nothing
    // (five-programs-37), and a patched peer does not ask.
    ingest(&b, PRIVATE, a_private.read().unwrap().0);
    exchange(&a, &b, KV).await;

    // The rail took both — B holds the private line on disk. If this fails
    // the test below proves nothing, because nothing arrived.
    assert_eq!(
        b.rail.journal(PRIVATE).unwrap().read().unwrap().0.len(),
        1,
        "the ingest is author-blind and namespace-blind, and that is the design"
    );

    b.project_dirty().await;
    b.project_all_on_disk().await;

    assert_eq!(
        value_at(&b, PRIVATE, "secret"),
        None,
        "a private namespace a peer pushed reached no reader"
    );
    assert_eq!(
        value_at(&b, KV, "public").as_deref(),
        Some(&b"shared"[..]),
        "control: an ordinary namespace over the same route in the same test did land"
    );
}

/// **(f) Retention on a rail-backed namespace stays retained.**
///
/// RED-FIRST, and the direction is the whole point: the store is a
/// PROJECTION now, so a row a local sweep deletes has no incumbent and the
/// next fold's `merge_entry` puts it straight back from the journal. The
/// sweep runs on a 60s-ish cadence and the fold runs on every tick, so a
/// 30-day retention window on a meshed node would be undone within a
/// minute, every minute, forever.
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    let now = commonwealth_core::clock::unix_now_secs();
    let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
    let floor = now - window;
    // Two ledger events A signed: one a day past the aggregation window,
    // one inside it. Days apart from the boundary, so no clock tick
    // between the plant and the sweep can move which side either is on.
    let a_journal = a.rail.journal(LEDGER).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[
                kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
            ])
            .unwrap(),
        2
    );

    exchange(&a, &b, LEDGER).await;
    b.project_all_on_disk().await;
    assert!(
        value_at(&b, LEDGER, "fresh-event").is_some(),
        "control: the ledger replicated at all"
    );

    // B's retention sweep, at the ONE cutoff the namespace's readers use.
    b.store.gc_app_before(LEDGER, floor).unwrap();
    assert_eq!(
        value_at(&b, LEDGER, "old-event"),
        None,
        "control: the sweep did delete the row"
    );

    // …and now one more round of the very thing that fills the store.
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, LEDGER, "old-event"),
        None,
        "the projection put back a row retention had just taken"
    );
    assert!(
        value_at(&b, LEDGER, "fresh-event").is_some(),
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
    let a = kv_node(da.path(), &ka, a_id, &kv_mesh(&ka, &kb, a_id, b_id));

    let now = commonwealth_core::clock::unix_now_secs();
    let window = u64::from(commonwealth_core::contributions::DEFAULT_WINDOW_DAYS) * 86_400;
    let floor = now - window;
    let a_journal = a.rail.journal(LEDGER).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[
                kv_op(LEDGER, &ka, 0, "old-event", Some(b"1"), floor - 86_400),
                kv_op(LEDGER, &ka, 1, "fresh-event", Some(b"1"), now - 60),
            ])
            .unwrap(),
        2
    );
    a.project_all_on_disk().await;
    assert!(
        value_at(&a, LEDGER, "fresh-event").is_some(),
        "control: A folded its own journal"
    );

    a.store.gc_app_before(LEDGER, floor).unwrap();
    assert_eq!(
        a.store.outbox_len().unwrap(),
        0,
        "an expiry is not a delete: it publishes nothing, because every \
             node derives the same floor from the same `t`"
    );

    a.project_all_on_disk().await;
    assert_eq!(
        value_at(&a, LEDGER, "old-event"),
        None,
        "A's own journal put back a row A's own retention had just taken"
    );
    assert!(
        value_at(&a, LEDGER, "fresh-event").is_some(),
        "and it took only the expired one"
    );
}
