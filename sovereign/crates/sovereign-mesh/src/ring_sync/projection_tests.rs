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

use super::tests::*;

// ── The mesh store as a projection of the rail (cw-lift 4) ──
//
// Two nodes, the REAL internal router, the REAL pump and the REAL fold.
// Every helper below is deliberately built from the production pieces:
// a fixture that appended its own ops or projected with its own roster
// would pass whatever the two halves happened to agree on.

/// The namespace these tests replicate. A `MeshStore` app_id verbatim, and
/// one of `DAEMON_OWN_NAMESPACES` — a namespace not on that list has no
/// derived roster and would be refused at the door for that reason alone,
/// which is a different test.
pub(super) const KV: &str = commonwealth_state::store_adapter::INFERENCE_APP_ID;

/// One mesh both nodes see. Each member carries the pubkey of the key its
/// node signs with, because that equality is the whole bridge between a
/// signature and a `NodeId` — a fixture whose membership is empty makes
/// every projected row `unattributed` for that reason and nothing else.
pub(super) fn kv_mesh(
    ka: &SigningKey,
    kb: &SigningKey,
    a: NodeId,
    b: NodeId,
) -> commonwealth_core::mesh::Mesh {
    use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
    mesh_of(vec![
        member(a, "a", Some(pubkey_of(ka))),
        member(b, "b", Some(pubkey_of(kb))),
    ])
}

/// A node whose rail derives its roster from that membership — what the
/// daemon installs, through the same call the daemon makes.
pub(super) fn kv_node(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: commonwealth_core::mesh::Mesh,
) -> (AppState, Arc<RingRail>) {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    let state = AppState::new_with_platform_and_engine_and_gauge_and_fabric(
        self_id,
        mesh,
        Arc::new(commonwealth_state::MeshStore::in_memory().unwrap()),
        Arc::new(sovereign_meshapp_registry::registry::AppRegistry::new()),
        None,
        None,
        sovereign_api::state::FabricSeed {
            ring_rail: Some(rail.clone()),
            ..Default::default()
        },
    );
    crate::ring_roster::MeshRosterSource::install(
        &rail,
        &state.inner.fabric.mesh,
        &state.inner.fabric.identity,
        state.self_node_pubkey(),
    )
    .unwrap();
    (state, rail)
}

/// One store write as the act a node would have signed, with the write
/// time chosen by the caller. The pump builds these from the outbox; these
/// are for the history a test needs to already exist.
pub(super) fn kv_op(
    ns: &str,
    key: &SigningKey,
    seq: u64,
    k: &str,
    v: Option<&[u8]>,
    t: u64,
) -> Op<SignedOp> {
    signed_in(
        ns,
        key,
        seq,
        RailAct::Record {
            payload: commonwealth_state::rail_kv::to_payload(k, v, t).unwrap(),
        },
    )
}

/// The floor named by the snapshot mark on these ops, if one is there.
pub(super) fn mark_of(ops: &[Op<SignedOp>]) -> Option<u64> {
    ops.iter().find_map(|o| match &o.kind.act {
        RailAct::Record { payload } => commonwealth_state::rail_kv::read_snapshot_mark(payload),
        _ => None,
    })
}

/// Whether these ops carry a store write for `key` — a tombstone included.
/// Used to assert that a delete really is OFF a disk, rather than trusting
/// a line count to have taken the right line.
pub(super) fn carries_write_for(ops: &[Op<SignedOp>], key: &str) -> bool {
    ops.iter().any(|o| match &o.kind.act {
        RailAct::Record { payload } => {
            commonwealth_state::rail_kv::from_payload(payload).is_some_and(|kv| kv.key == key)
        }
        _ => false,
    })
}

/// The 2,000 superseded writes that trip `SEAL_AFTER_OWN_OPS`, signed by
/// `key` from `first_seq`. Old `t`s, so nothing here can win a key.
pub(super) fn filler(k: &SigningKey, first_seq: u64, keys: &[&str]) -> Vec<Op<SignedOp>> {
    let old = commonwealth_core::clock::unix_now_secs() - 10_000;
    (0..crate::rail_kv_pump::SEAL_AFTER_OWN_OPS as u64)
        .map(|i| {
            kv_op(
                KV,
                k,
                first_seq + i,
                keys[i as usize % keys.len()],
                Some(format!("old-{i}").as_bytes()),
                old + i,
            )
        })
        .collect()
}

pub(super) fn value_at(state: &AppState, app_id: &str, key: &str) -> Option<Vec<u8>> {
    state
        .inner
        .fabric
        .mesh_store
        .get(app_id, key)
        .unwrap()
        .map(|e| e.value.to_vec())
}

/// **(a) A local store write reaches a peer's store, over the ring.**
///
/// The whole mechanism end to end: `set` queues, the pump signs, the
/// exchange carries, the fold projects. The origin assertion is the one
/// that cannot be faked — B never sees a `NodeId` on the wire, only a
/// signature, and the roster is what turns one into the other.
///
/// Watched RED by deleting the `journal.append` arm's `acked.push(row.id)`
/// and returning before the append: `pumped.appended` is 0 and B's store
/// answers `None`.
#[tokio::test]
async fn a_local_store_write_reaches_a_peers_store_through_the_ring() {
    let (ka, kb) = (
        SigningKey::from_bytes(&[1u8; 32]),
        SigningKey::from_bytes(&[2u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(1), NodeId::from_u128(2));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, _b_rail) = kv_node(db.path(), &kb, b_id, mesh);

    assert!(a_state
        .inner
        .fabric
        .mesh_store
        .set(KV, "plan", bytes::Bytes::from_static(b"v1"), a_id)
        .unwrap());
    assert_eq!(
        a_state.inner.fabric.mesh_store.outbox_len().unwrap(),
        1,
        "a local write queues for the rail"
    );

    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 1, "{pumped:?}");
    assert_eq!(pumped.deferred + pumped.refused, 0, "{pumped:?}");
    assert_eq!(
        a_state.inner.fabric.mesh_store.outbox_len().unwrap(),
        0,
        "an appended row is acked"
    );

    let url = serve(internal_router(b_state.clone())).await;
    let journal = a_rail.journal(KV).unwrap();
    let out = exchange(&reqwest::Client::new(), &url, &a_rail, &journal).await;
    assert!(out.stop.is_none(), "the exchange failed: {:?}", out.stop);
    assert_eq!(out.pushed, 1, "the write landed on the peer's journal");

    assert_eq!(
        crate::rail_kv_pump::project_all_on_disk(&b_state).await,
        1,
        "the peer folds the namespace it just received"
    );
    let got = b_state
        .inner
        .fabric
        .mesh_store
        .get(KV, "plan")
        .unwrap()
        .expect("the peer's store holds the write");
    assert_eq!(got.value.as_ref(), b"v1");
    assert_eq!(
        got.origin, a_id,
        "the origin comes from the roster placing a signature, never from \
             anything the sender supplied"
    );
}

/// **(b) A delete travels, and an older write does not undo it.**
///
/// Two claims in one journal, because they are the same claim: the fold
/// orders on the payload's `t`, so a tombstone at `t` beats every write
/// below it no matter when the line arrived. The lower-`t` write here is
/// signed by the OTHER node, which is the case an arrival-order rule
/// cannot get right.
///
/// Watched RED by folding on `op.ts_unix` instead of the payload's `t` in
/// `rail_kv::project`: B's store gets `stale` back and the final assertion
/// fails.
#[tokio::test]
async fn a_delete_travels_and_an_older_write_does_not_resurrect_the_key() {
    let (ka, kb) = (
        SigningKey::from_bytes(&[3u8; 32]),
        SigningKey::from_bytes(&[4u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(11), NodeId::from_u128(12));
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mesh = kv_mesh(&ka, &kb, a_id, b_id);
    let (a_state, a_rail) = kv_node(da.path(), &ka, a_id, mesh.clone());
    let (b_state, b_rail) = kv_node(db.path(), &kb, b_id, mesh);
    let now = commonwealth_core::clock::unix_now_secs();

    // A already holds the key, from a write older than the wall clock —
    // so the delete below is unambiguously later. `set` stamps `now`, and
    // a set/delete pair inside one second is a tie the fold breaks by
    // actor and id rather than by intent.
    let a_journal = a_rail.journal(KV).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[kv_op(KV, &ka, 0, "k", Some(b"live"), now - 100)])
            .unwrap(),
        1
    );
    crate::rail_kv_pump::project_all_on_disk(&a_state).await;
    assert_eq!(value_at(&a_state, KV, "k").as_deref(), Some(&b"live"[..]));

    let b_url = serve(internal_router(b_state.clone())).await;
    let client = reqwest::Client::new();
    let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "k").as_deref(),
        Some(&b"live"[..]),
        "control: the key reached the peer before it was deleted"
    );

    // The delete, through the store, through the pump, over the ring.
    assert!(a_state.inner.fabric.mesh_store.delete(KV, "k").unwrap());
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 1, "the tombstone is an act like any other");
    let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "k"),
        None,
        "the peer lost the key the tombstone names"
    );

    // A write B signed, older than the tombstone, arriving after it.
    let b_journal = b_rail.journal(KV).unwrap();
    assert_eq!(
        b_journal
            .ingest_all(&[kv_op(KV, &kb, 0, "k", Some(b"stale"), now - 50)])
            .unwrap(),
        1
    );
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "k"),
        None,
        "a lower-t write does not resurrect a deleted key"
    );
    // …and it does not resurrect it on the node that deleted it either,
    // once the op gets there.
    let out = exchange(&client, &b_url, &a_rail, &a_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    assert_eq!(out.pulled, 1, "B's older write came over");
    crate::rail_kv_pump::project_all_on_disk(&a_state).await;
    assert_eq!(value_at(&a_state, KV, "k"), None);
}

/// **(c) A seal bounds the ring, and the snapshot keeps every live key.**
///
/// The filler here is SUPERSEDED history of the same four keys — 2,000
/// older writes the live set has already overwritten, which is exactly
/// what a seal is for. Afterwards the peer's disk holds the seal and the
/// snapshot and nothing else, and its store still answers for every key.
///
/// The assertion is on what is on DISK and on what the STORE answers, not
/// on a return value: a seal that retired nothing, or a snapshot that
/// dropped the live set, would leave `PumpOutcome` looking identical.
///
/// Watched RED by deleting the `snapshot()` call from `seal_if_due`: the
/// disk assertion passes (one line, the seal) and every value assertion
/// below it fails — the seal became a delete.
#[tokio::test]
async fn a_seal_bounds_the_ring_and_the_snapshot_keeps_every_live_key() {
    const KEYS: [&str; 4] = ["k0", "k1", "k2", "k3"];
    let (ka, kb) = (
        SigningKey::from_bytes(&[5u8; 32]),
        SigningKey::from_bytes(&[6u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(21), NodeId::from_u128(22));
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
    // ── The control: four ops is not two thousand, and nothing seals.
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 4);
    assert_eq!(
        (pumped.sealed, pumped.snapshot_rows),
        (0, 0),
        "below the threshold the pump seals nothing"
    );
    let a_journal = a_rail.journal(KV).unwrap();
    assert_eq!(a_journal.read().unwrap().0.len(), 4);

    // ── 2,000 older writes of the same keys: history, already superseded.
    let filler = filler(&ka, 4, &KEYS);
    let total = 4 + filler.len();
    assert_eq!(a_journal.ingest_all(&filler).unwrap(), filler.len());

    // The peer takes the whole history first, so the prune below has
    // something to remove — otherwise "B's disk is short" would be true
    // because B was never told anything.
    let a_url = serve(internal_router(a_state.clone())).await;
    let client = reqwest::Client::new();
    let b_journal = b_rail.journal(KV).unwrap();
    let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    assert_eq!(
        b_journal.read().unwrap().0.len(),
        total,
        "control: the peer holds the unsealed history"
    );

    // ── One more local write, and the seal fires on the same tick.
    assert!(a_state
        .inner
        .fabric
        .mesh_store
        .set(KV, "k0", bytes::Bytes::from_static(b"newest"), a_id)
        .unwrap());
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 1);
    assert_eq!(pumped.sealed, 1, "{pumped:?}");
    assert_eq!(
        pumped.snapshot_rows,
        KEYS.len(),
        "every live row is re-appended above the new floor"
    );
    let held = a_journal.read().unwrap().0;
    assert_eq!(
        held.len(),
        1 + KEYS.len() + 1,
        "the writer's own disk is the seal, the snapshot, and the mark that \
             closes it: {held:?}"
    );
    assert!(matches!(held[0].kind.act, RailAct::Seal));
    assert_eq!(
        mark_of(&held),
        Some(held[0].kind.seq),
        "the snapshot ends with the mark naming the seal it completes — \
             without it no peer may retire anything of ours"
    );

    // ── The peer meets the seal and retires the same prefix.
    let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    assert_eq!(
        b_journal.read().unwrap().0.len(),
        1 + KEYS.len() + 1,
        "the peer's disk holds only the seal, the snapshot and its mark"
    );
    assert_eq!(
        a_journal.digest().unwrap(),
        b_journal.digest().unwrap(),
        "two nodes, one claim"
    );

    // ── …and every live key is still readable on the peer.
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "k0").as_deref(),
        Some(&b"newest"[..]),
        "the newest write survives its own snapshot"
    );
    for k in &KEYS[1..] {
        assert_eq!(
            value_at(&b_state, KV, k).as_deref(),
            Some(format!("live-{k}").as_bytes()),
            "{k} did not survive the seal"
        );
    }
}

/// **(c2) A delete keeps travelling past the seal that retired it.**
///
/// The cost ea4da7b68 recorded and priced rather than paid: a snapshot
/// carries LIVE rows, a tombstone is not one, so a peer that was away for
/// the delete used to keep the value forever — the KV shape of K7. Here B
/// holds all three keys, A deletes one while B is not listening, and the
/// seal that fires on the same tick takes the tombstone off A's disk before
/// any exchange could carry it. The middle assertion is the one that makes
/// this a real reproduction rather than a slow round: the delete is
/// provably UNREACHABLE, not merely late.
///
/// What B has afterwards is the seal, the snapshot and its mark — and that
/// is a claim about A's WHOLE live set, which is what retires the row.
///
/// Watched RED by dropping the reconciliation loop from
/// `MeshStore::apply_projection`: every assertion above the last passes and
/// B keeps `gone` at its stale value.
#[tokio::test]
async fn a_seal_carries_a_delete_the_peer_never_received() {
    const KEYS: [&str; 3] = ["k0", "k1", "gone"];
    let (ka, kb) = (
        SigningKey::from_bytes(&[15u8; 32]),
        SigningKey::from_bytes(&[16u8; 32]),
    );
    let (a_id, b_id) = (NodeId::from_u128(61), NodeId::from_u128(62));
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
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!((pumped.appended, pumped.sealed), (3, 0), "{pumped:?}");

    // B takes the whole live set, including the key that is about to go.
    let a_url = serve(internal_router(a_state.clone())).await;
    let client = reqwest::Client::new();
    let a_journal = a_rail.journal(KV).unwrap();
    let b_journal = b_rail.journal(KV).unwrap();
    let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;
    assert_eq!(
        value_at(&b_state, KV, "gone").as_deref(),
        Some(&b"live-gone"[..]),
        "control: the peer held the key before it was deleted"
    );

    // ── A deletes it, and seals on the same tick. B is not listening.
    assert_eq!(
        a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap(),
        crate::rail_kv_pump::SEAL_AFTER_OWN_OPS
    );
    assert!(a_state.inner.fabric.mesh_store.delete(KV, "gone").unwrap());
    let pumped = crate::rail_kv_pump::pump_once(&a_state).await;
    assert_eq!(pumped.appended, 1, "the tombstone was appended: {pumped:?}");
    assert_eq!(pumped.sealed, 1, "{pumped:?}");
    assert_eq!(pumped.snapshot_rows, 2, "two live rows, not three");

    let held = a_journal.read().unwrap().0;
    assert!(
        !carries_write_for(&held, "gone"),
        "THE REPRODUCTION: the tombstone is below the floor and off the \
             disk, so no exchange can ever carry it to B: {held:?}"
    );
    assert_eq!(mark_of(&held), Some(held[0].kind.seq));

    // ── The round that meets the seal.
    let out = exchange(&client, &a_url, &b_rail, &b_journal).await;
    assert!(out.stop.is_none(), "{:?}", out.stop);
    assert!(
        !carries_write_for(&b_journal.read().unwrap().0, "gone"),
        "B never receives a delete for it — the seal is what says so"
    );
    crate::rail_kv_pump::project_all_on_disk(&b_state).await;

    assert_eq!(
        value_at(&b_state, KV, "gone"),
        None,
        "the key A stopped asserting is gone from the peer that never saw \
             the tombstone"
    );
    for k in ["k0", "k1"] {
        assert_eq!(
            value_at(&b_state, KV, k).as_deref(),
            Some(format!("live-{k}").as_bytes()),
            "{k} was in the snapshot and must survive the same pass"
        );
    }
}
