// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replication — what a node tells a peer it has, and what the peer sends back.
//!
//! # Why this is not the mesh store's snapshot push
//!
//! The existing app-state replication ships a **full snapshot to every online
//! peer every ten seconds** (`gossip.rs` Step 4, 8,640 rounds/day). A household
//! writes on the order of 3,500 ops a year — call it 1.5 MB — so riding that
//! path would cost roughly **246 GB/day of egress per node**, and would tax
//! every other namespace on the same push forever. Bandwidth is the binding
//! constraint here and it binds on day one, not at scale.
//!
//! So the journal gets its own, slower push (sixty seconds is ample for money)
//! and syncs by **digest**, not by snapshot: about 600 bytes regardless of how
//! long the journal is.
//!
//! # The digest is a CONTIGUOUS high-water mark, and that word is load-bearing
//!
//! Per actor, the highest `n` such that this node holds every op `0..=n` from
//! them — **not** the highest seq it holds. A maximum cannot express a hole: a
//! node holding seq 0 and 2 would advertise `2`, the peer would answer
//! "nothing above 2", and seq 1 would never arrive. It would sit in the
//! admission report as a permanent [`SequenceHole`](crate::RailGap::SequenceHole)
//! while both nodes believed they were in sync.
//!
//! An actor missing from the digest means "I hold nothing of theirs", so the
//! peer sends everything. Absence is a request, never a default.
//!
//! # Everyone republishes everything they hold
//!
//! [`ops_missing_from`] does not filter by author. A node that has been
//! offline gets its ops back from whoever holds them, and a housemate who
//! leaves the ring does not take their half of the journal with them. This is
//! also why there is no own-origin skip to get wrong: the mesh store's
//! `origin` field names the last republisher rather than the author, which is
//! a trap for anyone syncing through it — this path has no origin field at
//! all, because the op carries its author in a signature.

use std::collections::{BTreeMap, BTreeSet};

use oplog::Op;

use crate::SignedOp;

/// Per-actor contiguous high-water marks. `{actor_pubkey_hex → n}`.
pub type Digest = BTreeMap<String, u64>;

/// What this node can honestly claim to hold, per actor.
///
/// See the module docs: this is the **contiguous** mark, so a hole below the
/// maximum lowers it and the peer re-sends from there. Healing a hole is
/// therefore automatic rather than a separate repair path.
pub fn digest(ops: &[Op<SignedOp>]) -> Digest {
    let mut by_actor: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for op in ops {
        by_actor
            .entry(op.actor.as_str())
            .or_default()
            .insert(op.kind.seq);
    }
    by_actor
        .into_iter()
        .filter_map(|(actor, seqs)| {
            // Absent from the digest is the honest answer when seq 0 itself is
            // missing — "I have some of their ops but not from the start" is
            // indistinguishable from "I have none" for the purpose of asking.
            if !seqs.contains(&0) {
                return None;
            }
            let mut n = 0u64;
            while seqs.contains(&(n + 1)) {
                n += 1;
            }
            Some((actor.to_string(), n))
        })
        .collect()
}

/// Every op this node holds that `theirs` says the peer is missing.
///
/// Author-blind on purpose (see the module docs). Ordered by
/// `(actor, seq)` so the wire payload is deterministic — two nodes with the
/// same holdings produce byte-identical bodies, which makes a diff of two
/// captures mean something.
pub fn ops_missing_from(ops: &[Op<SignedOp>], theirs: &Digest) -> Vec<Op<SignedOp>> {
    let mut out: Vec<Op<SignedOp>> = ops
        .iter()
        .filter(|op| match theirs.get(&op.actor) {
            Some(high_water) => op.kind.seq > *high_water,
            None => true,
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| (&a.actor, a.kind.seq).cmp(&(&b.actor, b.kind.seq)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{key, record, signed};

    fn actor(seed: u8) -> String {
        crate::actor_of(&key(seed))
    }

    fn op(seed: u8, seq: u64, ts: i64) -> Op<SignedOp> {
        signed(&key(seed), ts, seq, record("x"))
    }

    #[test]
    fn a_contiguous_run_reports_its_last_seq() {
        let ops = vec![op(1, 0, 10), op(1, 1, 11), op(1, 2, 12)];
        assert_eq!(digest(&ops), Digest::from([(actor(1), 2)]));
    }

    /// **The reason the mark is contiguous.** A node holding 0 and 2 must not
    /// claim 2, or the peer answers "nothing above 2" and seq 1 is lost
    /// forever while both sides believe they are in sync.
    #[test]
    fn a_hole_lowers_the_mark_so_the_peer_re_sends_across_it() {
        let held = vec![op(1, 0, 10), op(1, 2, 12)];
        assert_eq!(digest(&held), Digest::from([(actor(1), 0)]));

        let peer_has = vec![op(1, 0, 10), op(1, 1, 11), op(1, 2, 12)];
        let sent = ops_missing_from(&peer_has, &digest(&held));
        assert_eq!(
            sent.iter().map(|o| o.kind.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "the hole and everything above it come back"
        );
    }

    /// Missing seq 0 means we cannot make any contiguous claim, so we ask for
    /// everything rather than quietly claiming a mark we do not have.
    #[test]
    fn an_actor_whose_first_op_is_missing_is_absent_from_the_digest() {
        let held = vec![op(1, 3, 13)];
        assert!(digest(&held).is_empty());
        let peer_has = vec![op(1, 0, 10), op(1, 1, 11), op(1, 3, 13)];
        assert_eq!(ops_missing_from(&peer_has, &digest(&held)).len(), 3);
    }

    /// An actor we have never heard of is absent, and absence asks for
    /// everything rather than defaulting to zero (which would ask for
    /// everything ABOVE zero and silently skip their first op).
    #[test]
    fn an_unknown_actor_gets_their_whole_history() {
        let peer_has = vec![op(2, 0, 10), op(2, 1, 11)];
        assert_eq!(ops_missing_from(&peer_has, &Digest::new()).len(), 2);
    }

    #[test]
    fn a_peer_that_is_already_caught_up_is_sent_nothing() {
        let ops = vec![op(1, 0, 10), op(1, 1, 11)];
        assert!(ops_missing_from(&ops, &digest(&ops)).is_empty());
    }

    /// Replication is author-blind: a node republishes what it HOLDS, so a
    /// housemate who leaves does not take their half of the journal with them.
    #[test]
    fn a_node_republishes_ops_it_did_not_author() {
        let departed = op(3, 0, 10);
        let held = vec![op(1, 0, 11), departed.clone()];
        let sent = ops_missing_from(&held, &Digest::from([(actor(1), 0)]));
        assert_eq!(sent, vec![departed], "someone else's op, still republished");
    }

    /// Two nodes with the same holdings must produce the same bytes, or a
    /// captured payload diff means nothing.
    #[test]
    fn the_payload_order_is_content_derived_not_arrival_order() {
        let a = vec![op(1, 0, 10), op(2, 0, 11), op(1, 1, 12)];
        let b = vec![op(1, 1, 12), op(1, 0, 10), op(2, 0, 11)];
        assert_eq!(
            ops_missing_from(&a, &Digest::new()),
            ops_missing_from(&b, &Digest::new())
        );
    }
}
