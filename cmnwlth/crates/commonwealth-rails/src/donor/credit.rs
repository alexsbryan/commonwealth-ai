// SPDX-License-Identifier: AGPL-3.0-or-later
//! The credit a completed unit earns this node's contribution ledger
//! (cw-lift 5h). A sibling of `donor.rs` so it stays out of ARCH §3.1's
//! approach band.
use super::*;

/// What this node's contribution ledger owes itself for a unit it just
/// reported, or `None` when it owes nothing.
///
/// # Why the DONOR emits this, and not every node that folds the `Complete`
///
/// The other candidate looked like the convergent one and is written down
/// here because it is the argument, not the conclusion, that the next reader
/// needs: every node emits a credit when its own fold applies a `Complete`,
/// so the ledger becomes a function of the journal exactly the way the queue
/// is — the queue is a fold over admission, `svrn job status` folds the same
/// acts on any node, and a third replicated derivation of the same journal
/// would be in good company. It is still the wrong shape here, for two
/// reasons that are mechanism rather than taste.
///
/// **The contributions ledger is ALREADY a replicated log, and it converges
/// by "one write site, one event".** `LedgerEvent`s are stored in the mesh store
/// under the `contributions` app id, gossip to peers, and merge LWW on a key
/// of `origin:secs:nanos:seq` (`commonwealth-state`'s `contributions`). N nodes
/// folding one `Complete` would therefore write N rows under N DISTINCT keys,
/// and `aggregate` sums rows: one donated shard would be credited once per
/// ring member, and the error would grow with the ring. Making that safe
/// needs the credit keyed by `unit_hash` — a second idempotence key beside
/// the one the work fold already owns, which is the two-deciders defect ARCH
/// §10.6 names. Fold-emission would not make this ledger more convergent; it
/// would multiply one fact by the membership.
///
/// **A folding third party cannot produce this event anyway.** It can see
/// THAT the unit completed — that part is on the rail, signed and totally
/// ordered, and that is precisely why the rail is where the *fact* lives. It
/// cannot see `wall_seconds`: the journal carries `leased_at_ms` and
/// `completed_at_ms`, whose difference is lease-held time including journal
/// admit latency and heartbeat scheduling, not the compute. Only the machine
/// that ran the process measured that. This is the same reason
/// `InferenceServed` is emitted by the server, `KnowledgeQueryServed` by the
/// node that served it and `StorageSnapshot` by the host: in this ledger the
/// OBSERVER emits, once. On the work plane the donor is the observer.
///
/// What the rail keeps is the AUDIT. `handoff` + `unit_hash` + `donor_actor`
/// point at the signed `Complete` that has to exist for the credit to be
/// honest, and `donor_actor` is the [`ActorKey`] admission verified rather
/// than the self-reported `node_id` the emitter stamps (ARCH §7.5) — so the
/// two halves of the record can be checked against each other, which neither
/// could alone (ARCH §18.1: a claim asserted only on a field its own subject
/// supplies is not evidence).
///
/// # Double counting, under at-least-once delivery
///
/// The rail delivers a `Complete` at least once, and the fold is idempotent
/// per `unit_hash`: `WorkProjection::complete` moves `Leased -> Complete`
/// exactly once and every repeat lands on `double_deliveries` without
/// changing state. This credit is not derived from that stream at all — it is
/// written once, by the single process that ran the unit, in the arm where
/// that process's own `append` returned `Ok`. Redelivery cannot multiply it
/// because redelivery never reaches it, and a peer replaying the journal
/// emits nothing.
///
/// A unit whose report lapsed and which is re-leased and re-run IS credited
/// twice, to two different donors. That is not a double count: two machines
/// really did spend the time, and the ledger's job is to say so.
///
/// # `Complete` is credited and `Fail` is not
///
/// `WorkUnitStatus`' own distinction, kept: `Complete` is "the unit ran and
/// reached a verdict" — a red test shard included, since the unit did its job
/// — while `Fail` is "the plane failed to run the work". Crediting the second
/// would pay a donor for burning the submitter's attempts, which is a reward
/// pointed at exactly the wrong behaviour.
pub(super) fn credit_for(
    act: &WorkAct,
    unit: &JobUnit,
    self_key: &ActorKey,
    wall_seconds: f64,
) -> Option<commonwealth_core::contributions::LedgerEventKind> {
    match act {
        WorkAct::Complete(c) => Some(
            commonwealth_core::contributions::LedgerEventKind::JobUnitCompleted {
                handoff: c.handoff,
                unit_hash: c.unit_hash.clone(),
                // The key this node SIGNED with, taken from the rail signer
                // rather than from any field of the act — an actor is the one
                // thing on a journal line a writer cannot forge for somebody
                // else, and re-reading it off the payload would throw that
                // away.
                donor_actor: self_key.as_str().to_string(),
                kind: unit.kind.clone(),
                wall_seconds,
            },
        ),
        // `Fail` is the one other act `run_unit` builds, and it is uncredited
        // for the reason above. Nothing else can arrive here; a `Lease` or a
        // `Renew` is not a report, so "no credit" is the right answer for any
        // future act too rather than a hole this wildcard hides.
        _ => None,
    }
}
