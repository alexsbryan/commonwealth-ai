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
//! Per actor, the highest `n` such that this node needs nothing at or below
//! `n` — **not** the highest seq it holds. A maximum cannot express a hole: a
//! node holding seq 0 and 2 would advertise `2`, the peer would answer
//! "nothing above 2", and seq 1 would never arrive. It would sit in the
//! admission report as a permanent [`SequenceHole`](crate::RailGap::SequenceHole)
//! while both nodes believed they were in sync.
//!
//! An actor missing from the digest means "I hold nothing of theirs", so the
//! peer sends everything. Absence is a request, never a default.
//!
//! # The run starts at the sealed FLOOR, and that is what lets the rail delete
//!
//! "Contiguous from zero" is the same maximum problem one level down. A node
//! that retires an old prefix holds no seq 0, so it could claim nothing at
//! all — every peer read that as *I have none of theirs* and re-sent the whole
//! holding, every sixty seconds, forever. **Compaction amplified traffic and
//! then undid itself**, which is why the rail could not delete anything at any
//! granularity.
//!
//! So the run counts from a floor: the `seq` of the actor's own highest
//! [`Seal`](crate::RailAct::Seal), by [`sealed_floors`]. Nothing below it is
//! wanted, nothing below it is sent, and [`admit`](crate::admit) does not call
//! it a hole.
//!
//! **The floor is authored, never configured.** A local truncation setting
//! would put the disagreement back one layer up — two nodes with different
//! floors, one re-sending forever. A seal is a signed op in the same total
//! order as every other act, so peers agree about it by the mechanism they
//! already have, and the seal itself is how the floor travels.
//!
//! **The wire type does not change, and that is deliberate.** A peer uses the
//! digest for exactly one decision — which ops to send — and the answer is
//! `(mark, ∞)` whether the run started at zero or at a floor. Putting the
//! floor on the wire as well would add a second thing an old peer can fail to
//! parse and buy nothing; see the compatibility note on [`Digest`].
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

use crate::{RailAct, SignedOp};

/// Per-actor contiguous high-water marks. `{actor_pubkey_hex → n}`.
///
/// `n` means **"I need nothing at or below this"** — which is a strict
/// generalisation of the old reading ("I hold every op `0..=n`"), identical
/// whenever nothing is sealed. That is why the sealed floor did NOT become a
/// second field here: a peer's only use for this map is to answer *which ops
/// do I send*, and `(n, ∞)` is that answer either way.
///
/// It matters because this crosses the wire every sixty seconds between peers
/// that may be on different builds (`sovereign-mesh/src/ring_sync.rs`), and
/// disk and wire are different compatibility clocks. A digest written by a
/// build that knows about seals is byte-identical to one written by a build
/// that does not, for the same holding — so neither side can fail to read the
/// other, and there is nothing to default and no version to tag.
pub type Digest = BTreeMap<String, u64>;

/// Where each actor's history still starts. `{actor_pubkey_hex → seq}`.
pub(crate) type Floors = BTreeMap<String, u64>;

/// The ONE reading of a [`Seal`](crate::RailAct::Seal) (ARCH §10.6).
///
/// An actor's floor is the `seq` of their own highest seal, because a seal
/// retires everything its author wrote before it. Never having sealed is a
/// floor of zero, which is exactly the behaviour this rail had before seals
/// existed.
///
/// **Which ops you hand it is the whole safety question.** [`admit`](crate::admit)
/// hands it only ops that passed the signature and roster checks, so a forged
/// or unclaimed seal retires nothing — a seal is the one act that makes the
/// rail stop asking for history, and treating an unauthenticated one as
/// authoritative would let a single pushed line erase a member's past on every
/// node that received it (ARCH §18.3). [`digest`] hands it the whole holding,
/// which is the same trust the contiguous mark beside it has always had:
/// the digest says what to ASK for, and being wrong there costs a round, while
/// being wrong in `admit` states a total over a subset and calls it complete.
pub(crate) fn sealed_floors<'a>(ops: impl IntoIterator<Item = &'a Op<SignedOp>>) -> Floors {
    let mut floors = Floors::new();
    for op in ops {
        if matches!(op.kind.act, RailAct::Seal) {
            let floor = floors.entry(op.actor.clone()).or_insert(0);
            *floor = (*floor).max(op.kind.seq);
        }
    }
    floors
}

/// What this node can honestly claim to need nothing below, per actor.
///
/// See the module docs: this is the **contiguous** mark, so a hole below the
/// maximum lowers it and the peer re-sends from there. Healing a hole is
/// therefore automatic rather than a separate repair path. The run counts from
/// the actor's sealed floor, so a node that has compacted a retired prefix
/// still makes a claim instead of falling silent.
pub fn digest(ops: &[Op<SignedOp>]) -> Digest {
    let floors = sealed_floors(ops);
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
            // Absent from the digest is the honest answer when the floor
            // itself is missing — "I have some of their ops but not from
            // where their history starts" is indistinguishable from "I have
            // none" for the purpose of asking. A sealed actor cannot reach
            // this: the floor IS the seal's own seq, so holding the seal is
            // holding the floor.
            let mut n = floors.get(actor).copied().unwrap_or(0);
            if !seqs.contains(&n) {
                return None;
            }
            while seqs.contains(&(n + 1)) {
                n += 1;
            }
            Some((actor.to_string(), n))
        })
        .collect()
}

/// Every op this node holds that `theirs` says the peer is missing.
///
/// A mark of `n` means the peer needs nothing at or below `n`, sealed or held
/// alike, so honouring the floor is the same `>` this always was — the peer's
/// retired prefix is simply never above its own mark. Author-blind on purpose
/// (see the module docs). Ordered by
/// `(actor, seq)` so the wire payload is deterministic — two nodes with the
/// same holdings produce byte-identical bodies, which makes a diff of two
/// captures mean something.
///
/// This is [`ops_missing_from_within`] with no budget, and it is the honest
/// TOTAL: what a caller wants when it is measuring, not when it is sending.
/// Everything that puts ops on a wire uses the budgeted form.
pub fn ops_missing_from(ops: &[Op<SignedOp>], theirs: &Digest) -> Vec<Op<SignedOp>> {
    ops_missing_from_within(ops, theirs, NO_BUDGET).0
}

/// The budget that never binds — what [`ops_missing_from`] passes.
///
/// Named rather than spelled `usize::MAX` at the call site because it is also
/// the value the pricing fast path tests for: costing every op is a second
/// whole serialisation of the journal, and a caller that cannot be cut short
/// must not pay for it.
pub const NO_BUDGET: usize = usize::MAX;

/// Every op this node holds that `theirs` says the peer is missing, stopped
/// at `budget_bytes` of serialised ops.
///
/// Returns `(ops, more)`. **`more` is the whole reason this is not a
/// `Vec`**: "here is everything you lack" and "here is as much of it as fits"
/// are different facts, and a truncation the caller cannot see is a silent
/// substitution (ARCH §18.3). The receiver's body limit answers an oversized
/// exchange at the extractor, so nothing downstream of a quiet truncation
/// would ever have said otherwise.
///
/// # Why repeating this terminates (the sync loop's whole safety argument)
///
/// **The first op of a non-empty chunk is always one the peer does not
/// hold.** A mark of `n` for an actor means their run is contiguous THROUGH
/// `n`, so they do not hold `n + 1`; the selection filters on `seq > n` and
/// orders by `(actor, seq)`, so the lowest element it can yield is exactly
/// that op. A peer therefore ingests at least one new op per non-empty chunk,
/// both holdings are finite, and the caller's loop
/// (`sovereign-mesh/src/ring_sync.rs::exchange`) cannot spin against a peer
/// that is merely behind.
///
/// # The budget is in WIRE bytes, and one op is never split
///
/// Cost is `serde_json` length plus the array's separating comma — the same
/// serialisation the receiver's limit compares against, because any other
/// number would be a second implementation of "how big is this" (ARCH §10.6).
///
/// An op that alone exceeds the budget is still sent. A chunk of none would
/// be the spin this exists to prevent, and the honest failure for an op too
/// large for any exchange is the receiver's refusal — not a sender that
/// quietly stops.
pub fn ops_missing_from_within(
    ops: &[Op<SignedOp>],
    theirs: &Digest,
    budget_bytes: usize,
) -> (Vec<Op<SignedOp>>, bool) {
    let mut wanted: Vec<&Op<SignedOp>> = ops
        .iter()
        .filter(|op| match theirs.get(&op.actor) {
            Some(high_water) => op.kind.seq > *high_water,
            None => true,
        })
        .collect();
    wanted.sort_by(|a, b| (&a.actor, a.kind.seq).cmp(&(&b.actor, b.kind.seq)));

    if budget_bytes == NO_BUDGET {
        return (wanted.into_iter().cloned().collect(), false);
    }

    let mut out: Vec<Op<SignedOp>> = Vec::new();
    let mut spent: usize = 0;
    for op in wanted {
        let cost = match serde_json::to_vec(op) {
            // `+ 1` for the comma the enclosing array puts between elements.
            Ok(bytes) => bytes.len() + 1,
            // Unreachable for an op that came off a wire or out of a journal,
            // but a price that cannot be computed must not read as free:
            // charge the whole budget so such an op is only ever sent alone,
            // and let the receiver — not a silent drop — judge it.
            Err(_) => budget_bytes,
        };
        if !out.is_empty() && spent.saturating_add(cost) > budget_bytes {
            return (out, true);
        }
        spent = spent.saturating_add(cost);
        out.push(op.clone());
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{key, record, signed};
    use crate::RailAct;

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
    // ── the sealed floor ─────────────────────────────────

    fn seal(seed: u8, seq: u64, ts: i64) -> Op<SignedOp> {
        signed(&key(seed), ts, seq, RailAct::Seal)
    }

    /// **Truncation must not undo itself.** Before the floor existed,
    /// `digest` early-returned `None` for any actor whose seq 0 it did not
    /// hold — so a node that compacted a sealed prefix advertised NOTHING for
    /// that actor, every peer read that as "I have none of theirs", and the
    /// whole holding came back on the next sixty-second round. Compaction
    /// amplified traffic and then reversed itself.
    ///
    /// The first assertion is the negative control: the same holding with no
    /// seal in it really is unadvertisable, so the second cannot pass on a
    /// digest that was never empty.
    #[test]
    fn a_compacted_actor_is_advertised_from_its_seal_instead_of_not_at_all() {
        let no_seal = vec![op(1, 3, 103), op(1, 4, 104)];
        assert!(
            digest(&no_seal).is_empty(),
            "control: nothing sealed, nothing contiguous from 0, nothing to claim"
        );

        let compacted = vec![seal(1, 3, 103), op(1, 4, 104)];
        assert_eq!(
            digest(&compacted),
            Digest::from([(actor(1), 4)]),
            "the mark counts from the sealed floor, not from zero"
        );
    }

    /// **A peer must not re-send a sealed prefix, ever.** This is the same
    /// defect seen from the other end: the responder holds the retired ops
    /// and, reading a digest that could not name a floor, shipped all of them
    /// every round.
    #[test]
    fn a_peer_never_re_sends_what_the_seal_retired() {
        // The prefix the seal retires, and what is left after compaction. The
        // peer is a node that never compacted, so it holds both.
        let retired: Vec<_> = (0..3).map(|s| op(1, s, 100 + s as i64)).collect();
        let compacted = vec![seal(1, 3, 103), op(1, 4, 104)];
        let peer_holds: Vec<_> = retired.iter().chain(&compacted).cloned().collect();
        assert!(
            ops_missing_from(&peer_holds, &digest(&compacted)).is_empty(),
            "the retired prefix is not wanted and must not be sent"
        );

        // And the seal still travels to a peer that has not got it yet: a
        // node cannot honour a floor it has never seen.
        assert_eq!(
            ops_missing_from(&peer_holds, &digest(&retired))
                .iter()
                .map(|o| o.kind.seq)
                .collect::<Vec<_>>(),
            vec![3, 4],
            "the seal itself is an op, and it is how the floor propagates"
        );
    }

    #[test]
    fn the_payload_order_is_content_derived_not_arrival_order() {
        let a = vec![op(1, 0, 10), op(2, 0, 11), op(1, 1, 12)];
        let b = vec![op(1, 1, 12), op(1, 0, 10), op(2, 0, 11)];
        assert_eq!(
            ops_missing_from(&a, &Digest::new()),
            ops_missing_from(&b, &Digest::new())
        );
    }

    // ── the byte budget ──────────────────────────────────

    fn wire_bytes(ops: &[Op<SignedOp>]) -> usize {
        serde_json::to_vec(ops).unwrap().len()
    }

    /// The unbudgeted form is the budgeted one with a budget that cannot
    /// bind — one selection rule, not two (ARCH §10.6).
    #[test]
    fn the_unbudgeted_form_is_the_budgeted_one_with_no_budget() {
        let held: Vec<_> = (0..20).map(|s| op(1, s, 100 + s as i64)).collect();
        let (all, more) = ops_missing_from_within(&held, &Digest::new(), NO_BUDGET);
        assert_eq!(all, ops_missing_from(&held, &Digest::new()));
        assert!(!more, "a budget that cannot bind never truncates");
    }

    /// A budget larger than the whole selection changes nothing and says so.
    #[test]
    fn a_budget_nothing_reaches_sends_everything_and_reports_no_more() {
        let held: Vec<_> = (0..20).map(|s| op(1, s, 100 + s as i64)).collect();
        let whole = wire_bytes(&held);
        let (sent, more) = ops_missing_from_within(&held, &Digest::new(), whole * 2);
        assert_eq!(sent.len(), 20);
        assert!(!more);
    }

    /// **The truncation is visible, and it is the lowest ops that go.** A
    /// chunk that quietly dropped the tail would leave the peer with a hole
    /// it could never name.
    #[test]
    fn a_budget_that_binds_sends_the_lowest_ops_and_reports_more() {
        let held: Vec<_> = (0..20).map(|s| op(1, s, 100 + s as i64)).collect();
        let five = wire_bytes(&held[..5]);
        let (sent, more) = ops_missing_from_within(&held, &Digest::new(), five);
        assert!(more, "the budget cut it short and must say so");
        assert!(
            (1..20).contains(&sent.len()),
            "a budget of five ops' bytes sent {} of 20",
            sent.len()
        );
        assert_eq!(
            sent.iter().map(|o| o.kind.seq).collect::<Vec<_>>(),
            (0..sent.len() as u64).collect::<Vec<_>>(),
            "the chunk is the contiguous LOW end, so the peer's mark advances"
        );
        assert!(
            wire_bytes(&sent) <= five,
            "the chunk must fit the budget it was given"
        );
    }

    /// **A chunk of none would be the spin.** One op larger than the whole
    /// budget still goes out alone; the receiver's refusal is the honest
    /// failure, and a sender that returned nothing forever is not.
    #[test]
    fn an_op_bigger_than_the_whole_budget_is_still_sent_alone() {
        let held: Vec<_> = (0..3).map(|s| op(1, s, 100 + s as i64)).collect();
        let (sent, more) = ops_missing_from_within(&held, &Digest::new(), 1);
        assert_eq!(sent.len(), 1, "exactly one, never zero");
        assert_eq!(sent[0].kind.seq, 0);
        assert!(more);
    }

    /// **K9, in the small.** Repeating the chunked selection against a peer
    /// that ingests what it is sent converges, and the bound on the number of
    /// rounds is the count of ops rather than anything larger: every non-empty
    /// chunk starts at an op the peer provably lacks, so every round moves the
    /// peer's mark.
    #[test]
    fn repeating_a_budgeted_chunk_converges_and_every_round_moves_the_mark() {
        let held: Vec<_> = (0..40).map(|s| op(1, s, 100 + s as i64)).collect();
        let budget = wire_bytes(&held[..3]);
        let mut peer: Vec<Op<SignedOp>> = Vec::new();
        let mut rounds = 0;
        loop {
            let (chunk, more) = ops_missing_from_within(&held, &digest(&peer), budget);
            if chunk.is_empty() {
                assert!(!more, "nothing to send cannot also mean more remains");
                break;
            }
            let before = digest(&peer);
            peer.extend(chunk);
            assert_ne!(
                digest(&peer),
                before,
                "round {rounds} sent a chunk that moved nothing — this is the spin"
            );
            rounds += 1;
            assert!(rounds <= 40, "a 40-op journal must not need 40+ rounds");
        }
        assert_eq!(peer.len(), 40, "converged, in {rounds} rounds");
        assert_eq!(digest(&peer), digest(&held), "two nodes, one claim");
        assert!(rounds > 1, "the control: this budget really did chunk");
    }
}
