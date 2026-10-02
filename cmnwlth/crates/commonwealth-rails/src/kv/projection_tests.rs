// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh store as a projection of the rail, two nodes apart — moved from
//! sovereign-daemon's `ring_sync_projection_tests` and the pump test of
//! `rail_kv_pump_loop_tests` in five-programs fp-83, when the daemon pump's
//! KV half was deleted and this store became the one that pumps, seals and
//! folds.
//!
//! The assertions are verbatim. What changed is the fixture: each node is a
//! [`KvHost`] over its own rail, and [`exchange`] carries ops between two of
//! them through the doors the daemon's ring round reaches here — `missing`,
//! the ingest door (which marks the namespace for the next fold) and
//! `compact` when a seal arrived — rather than a second spelling of the ring.

use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_rail::{
    actor_of, body_json, sign_ring_op, Ed25519Verifier, Op, RailAct, RingRail, SignedOp,
    SigningKey, NO_BUDGET,
};
use commonwealth_state::rail_kv::{self, SEAL_AFTER_OWN_OPS};
use tokio::sync::RwLock;

use super::tests::{solo_mesh, ME};
use super::KvHost;
use crate::rail::{IngestBody, MembershipRosterSource};

/// The namespace these tests replicate. A `MeshStore` app_id verbatim, and
/// one of the daemon's own namespaces.
pub(super) const KV: &str = commonwealth_state::store_adapter::INFERENCE_APP_ID;

fn pubkey(key: &SigningKey) -> NodePubkey {
    commonwealth_transport::identity::node_pubkey(key)
}

/// One mesh both nodes see. Each member carries the pubkey of the key its
/// node signs with, because that equality is the whole bridge between a
/// signature and a `NodeId` — a fixture whose membership is empty makes
/// every projected row `unattributed` for that reason and nothing else.
pub(super) fn kv_mesh(ka: &SigningKey, kb: &SigningKey, a: NodeId, b: NodeId) -> Arc<RwLock<Mesh>> {
    let mesh = solo_mesh();
    {
        let mut m = mesh.try_write().expect("a fresh mesh has no reader");
        let template = m.members[&NodeId::from_u128(ME)].clone();
        m.members.clear();
        for (id, name, key) in [(a, "a", ka), (b, "b", kb)] {
            let mut record = template.clone();
            record.node_id = id;
            record.name = name.into();
            record.node_pubkey = Some(pubkey(key));
            m.members.insert(id, record);
        }
    }
    mesh
}

/// A node whose rail derives its roster from that membership — built the way
/// `RailsDaemon::start` builds it.
pub(super) fn kv_node(
    dir: &std::path::Path,
    key: &SigningKey,
    self_id: NodeId,
    mesh: &Arc<RwLock<Mesh>>,
) -> Arc<KvHost> {
    let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
    MembershipRosterSource::install(&rail, mesh, self_id, Some(pubkey(key)));
    Arc::new(KvHost::new(rail, mesh.clone(), self_id, Some(pubkey(key))).unwrap())
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
    let act = RailAct::Record {
        payload: rail_kv::to_payload(k, v, t).unwrap(),
    };
    let ts = 1_700_000_000i64 + seq as i64;
    let sig = sign_ring_op(key, ns, ts, seq, &body_json(&act, None, None));
    Op::new(
        SignedOp {
            seq,
            sig,
            act,
            on_behalf_of: None,
            view: None,
        },
        ts,
        actor_of(key),
    )
}

/// The floor named by the snapshot mark on these ops, if one is there.
pub(super) fn mark_of(ops: &[Op<SignedOp>]) -> Option<u64> {
    ops.iter().find_map(|o| match &o.kind.act {
        RailAct::Record { payload } => rail_kv::read_snapshot_mark(payload),
        _ => None,
    })
}

/// Whether these ops carry a store write for `key` — a tombstone included.
/// Used to assert that a delete really is OFF a disk, rather than trusting
/// a line count to have taken the right line.
pub(super) fn carries_write_for(ops: &[Op<SignedOp>], key: &str) -> bool {
    ops.iter().any(|o| match &o.kind.act {
        RailAct::Record { payload } => {
            rail_kv::from_payload(payload).is_some_and(|kv| kv.key == key)
        }
        _ => false,
    })
}

/// The 2,000 superseded writes that trip `SEAL_AFTER_OWN_OPS`, signed by
/// `key` from `first_seq`. Old `t`s, so nothing here can win a key.
pub(super) fn filler(k: &SigningKey, first_seq: u64, keys: &[&str]) -> Vec<Op<SignedOp>> {
    let old = commonwealth_core::clock::unix_now_secs() - 10_000;
    (0..SEAL_AFTER_OWN_OPS as u64)
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

pub(super) fn value_at(host: &KvHost, app_id: &str, key: &str) -> Option<Vec<u8>> {
    host.store
        .get(app_id, key)
        .unwrap()
        .map(|e| e.value.to_vec())
}

/// What one [`exchange`] moved.
#[derive(Debug, Default)]
pub(super) struct Exchanged {
    pub pulled: usize,
    pub pushed: usize,
}

/// Feed `ops` through `to`'s ingest door, as the ring round does; the door
/// marks the namespace for `to`'s next fold. Returns how many were new.
pub(super) fn ingest(to: &KvHost, ns: &str, ops: Vec<Op<SignedOp>>) -> usize {
    let journal = to.rail.journal(ns).unwrap();
    let before = journal.read().unwrap().0.len();
    let answer = crate::rail::ingest_answer(&journal, to, IngestBody { ops });
    assert!(answer.status().is_success(), "{:?}", answer.status());
    journal.read().unwrap().0.len().saturating_sub(before)
}

/// One ring exchange of `ns` between `local` and `peer`: `local` takes what
/// its digest says it lacks (and prunes to a seal it pulled), then gives
/// `peer` what `peer`'s digest says it lacks. The doors the daemon's ring
/// round calls against cw-rails, in the order it calls them.
pub(super) async fn exchange(local: &KvHost, peer: &KvHost, ns: &str) -> Exchanged {
    let mut out = Exchanged::default();
    let lj = local.rail.journal(ns).unwrap();
    let pj = peer.rail.journal(ns).unwrap();
    loop {
        let (theirs, _) = pj
            .ops_missing_from_within(
                &commonwealth_rail::Ed25519Verifier,
                &lj.digest(&commonwealth_rail::Ed25519Verifier).unwrap(),
                NO_BUDGET,
            )
            .unwrap();
        let sealed = theirs.iter().any(|o| matches!(o.kind.act, RailAct::Seal));
        let pulled = ingest(local, ns, theirs);
        if pulled > 0 && sealed {
            let roster = local.rail.roster(&lj).await.unwrap();
            lj.compact(&roster, &Ed25519Verifier).unwrap();
        }
        let (ours, _) = lj
            .ops_missing_from_within(
                &commonwealth_rail::Ed25519Verifier,
                &pj.digest(&commonwealth_rail::Ed25519Verifier).unwrap(),
                NO_BUDGET,
            )
            .unwrap();
        let pushed = ingest(peer, ns, ours);
        out.pulled += pulled;
        out.pushed += pushed;
        if pulled == 0 && pushed == 0 {
            return out;
        }
    }
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    assert!(a
        .store
        .set(KV, "plan", bytes::Bytes::from_static(b"v1"), a_id)
        .unwrap());
    assert_eq!(
        a.store.outbox_len().unwrap(),
        1,
        "a local write queues for the rail"
    );

    let pumped = a.pump_once().await;
    assert_eq!(pumped.appended, 1, "{pumped:?}");
    assert_eq!(pumped.deferred + pumped.refused, 0, "{pumped:?}");
    assert_eq!(a.store.outbox_len().unwrap(), 0, "an appended row is acked");

    let out = exchange(&a, &b, KV).await;
    assert_eq!(out.pushed, 1, "the write landed on the peer's journal");

    assert_eq!(
        b.project_all_on_disk().await,
        1,
        "the peer folds the namespace it just received"
    );
    let got = b
        .store
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);
    let now = commonwealth_core::clock::unix_now_secs();

    // A already holds the key, from a write older than the wall clock —
    // so the delete below is unambiguously later. `set` stamps `now`, and
    // a set/delete pair inside one second is a tie the fold breaks by
    // actor and id rather than by intent.
    let a_journal = a.rail.journal(KV).unwrap();
    assert_eq!(
        a_journal
            .ingest_all(&[kv_op(KV, &ka, 0, "k", Some(b"live"), now - 100)])
            .unwrap(),
        1
    );
    a.project_all_on_disk().await;
    assert_eq!(value_at(&a, KV, "k").as_deref(), Some(&b"live"[..]));

    exchange(&a, &b, KV).await;
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, KV, "k").as_deref(),
        Some(&b"live"[..]),
        "control: the key reached the peer before it was deleted"
    );

    // The delete, through the store, through the pump, over the ring.
    assert!(a.store.delete(KV, "k").unwrap());
    let pumped = a.pump_once().await;
    assert_eq!(pumped.appended, 1, "the tombstone is an act like any other");
    exchange(&a, &b, KV).await;
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, KV, "k"),
        None,
        "the peer lost the key the tombstone names"
    );

    // A write B signed, older than the tombstone, arriving after it.
    let b_journal = b.rail.journal(KV).unwrap();
    assert_eq!(
        b_journal
            .ingest_all(&[kv_op(KV, &kb, 0, "k", Some(b"stale"), now - 50)])
            .unwrap(),
        1
    );
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, KV, "k"),
        None,
        "a lower-t write does not resurrect a deleted key"
    );
    // …and it does not resurrect it on the node that deleted it either,
    // once the op gets there.
    let out = exchange(&a, &b, KV).await;
    assert_eq!(out.pulled, 1, "B's older write came over");
    a.project_all_on_disk().await;
    assert_eq!(value_at(&a, KV, "k"), None);
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    for k in KEYS {
        assert!(a
            .store
            .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
            .unwrap());
    }
    // ── The control: four ops is not two thousand, and nothing seals.
    let pumped = a.pump_once().await;
    assert_eq!(pumped.appended, 4);
    assert_eq!(
        (pumped.sealed, pumped.snapshot_rows),
        (0, 0),
        "below the threshold the pump seals nothing"
    );
    let a_journal = a.rail.journal(KV).unwrap();
    assert_eq!(a_journal.read().unwrap().0.len(), 4);

    // ── 2,000 older writes of the same keys: history, already superseded.
    let filler = filler(&ka, 4, &KEYS);
    let total = 4 + filler.len();
    assert_eq!(a_journal.ingest_all(&filler).unwrap(), filler.len());

    // The peer takes the whole history first, so the prune below has
    // something to remove — otherwise "B's disk is short" would be true
    // because B was never told anything.
    let b_journal = b.rail.journal(KV).unwrap();
    exchange(&b, &a, KV).await;
    assert_eq!(
        b_journal.read().unwrap().0.len(),
        total,
        "control: the peer holds the unsealed history"
    );

    // ── One more local write, and the seal fires on the same tick.
    assert!(a
        .store
        .set(KV, "k0", bytes::Bytes::from_static(b"newest"), a_id)
        .unwrap());
    let pumped = a.pump_once().await;
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
    exchange(&b, &a, KV).await;
    assert_eq!(
        b_journal.read().unwrap().0.len(),
        1 + KEYS.len() + 1,
        "the peer's disk holds only the seal, the snapshot and its mark"
    );
    assert_eq!(
        a_journal
            .digest(&commonwealth_rail::Ed25519Verifier)
            .unwrap(),
        b_journal
            .digest(&commonwealth_rail::Ed25519Verifier)
            .unwrap(),
        "two nodes, one claim"
    );

    // ── …and every live key is still readable on the peer.
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, KV, "k0").as_deref(),
        Some(&b"newest"[..]),
        "the newest write survives its own snapshot"
    );
    for k in &KEYS[1..] {
        assert_eq!(
            value_at(&b, KV, k).as_deref(),
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
    let a = kv_node(da.path(), &ka, a_id, &mesh);
    let b = kv_node(db.path(), &kb, b_id, &mesh);

    for k in KEYS {
        assert!(a
            .store
            .set(KV, k, bytes::Bytes::from(format!("live-{k}")), a_id)
            .unwrap());
    }
    let pumped = a.pump_once().await;
    assert_eq!((pumped.appended, pumped.sealed), (3, 0), "{pumped:?}");

    // B takes the whole live set, including the key that is about to go.
    let a_journal = a.rail.journal(KV).unwrap();
    let b_journal = b.rail.journal(KV).unwrap();
    exchange(&b, &a, KV).await;
    b.project_all_on_disk().await;
    assert_eq!(
        value_at(&b, KV, "gone").as_deref(),
        Some(&b"live-gone"[..]),
        "control: the peer held the key before it was deleted"
    );

    // ── A deletes it, and seals on the same tick. B is not listening.
    assert_eq!(
        a_journal.ingest_all(&filler(&ka, 3, &KEYS)).unwrap(),
        SEAL_AFTER_OWN_OPS
    );
    assert!(a.store.delete(KV, "gone").unwrap());
    let pumped = a.pump_once().await;
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
    exchange(&b, &a, KV).await;
    assert!(
        !carries_write_for(&b_journal.read().unwrap().0, "gone"),
        "B never receives a delete for it — the seal is what says so"
    );
    b.project_all_on_disk().await;

    assert_eq!(
        value_at(&b, KV, "gone"),
        None,
        "the key A stopped asserting is gone from the peer that never saw \
             the tombstone"
    );
    for k in ["k0", "k1"] {
        assert_eq!(
            value_at(&b, KV, k).as_deref(),
            Some(format!("live-{k}").as_bytes()),
            "{k} was in the snapshot and must survive the same pass"
        );
    }
}

/// **A node in no mesh queues its writes; it does not lose them.**
///
/// `NotInRoster` is the one refusal that is not a refusal — a solo daemon
/// is a normal daemon, and its writes travel the moment it joins. Dropping
/// them would make a legitimate condition permanent, and retrying an
/// append the rail will never accept would be the other failure. The
/// second half is the control: the same row, the same pump, one member
/// added.
#[tokio::test]
async fn a_node_in_no_mesh_keeps_its_writes_queued_until_membership_exists() {
    let dir = tempfile::tempdir().unwrap();
    // A mesh with nobody in it: this node cannot place its own key.
    let mesh = solo_mesh();
    let me = NodeId::from_u128(ME);
    let record = mesh.write().await.members.remove(&me).unwrap();
    let host = super::tests::host_at(dir.path(), &mesh);

    // Two rows, so the drain's one batch defers every row, not the first.
    for k in ["plan", "notes"] {
        assert!(host
            .store
            .set(KV, k, bytes::Bytes::from_static(b"v1"), me)
            .unwrap());
    }

    let out = host.pump_once().await;
    assert_eq!(
        (out.appended, out.deferred, out.refused),
        (0, 2, 0),
        "{out:?}"
    );
    assert_eq!(
        host.store.outbox_len().unwrap(),
        2,
        "a deferred write stays queued"
    );

    // Membership arrives, and the same rows go out.
    mesh.write().await.members.insert(me, record);
    let out = host.pump_once().await;
    assert_eq!(
        (out.appended, out.deferred, out.refused),
        (2, 0, 0),
        "{out:?}"
    );
    assert_eq!(host.store.outbox_len().unwrap(), 0);
}
