// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fold — what the `work` journal *means*, as a queue.
//!
//! [`WorkProjection::fold`] walks an [`Admission`] once and returns the state
//! of every handoff on this ring: which units exist, who holds which lease,
//! what has finished, and everything it could not account for. There is no
//! queue server and no lease table; a node that holds the journal holds the
//! queue, and two nodes holding the same journal compute the same queue.
//!
//! # Order-independence, and what it does and does not mean
//!
//! The fold reads [`act::read`], which reads `Admission::applied()` — the
//! total order `(ts_unix, actor, id)` every node already agrees on — and it
//! **adds no ordering of its own**. That is the whole property, and it has two
//! halves that fail differently:
//!
//! - **Arrival order does not matter.** Whatever sequence the ops reached this
//!   node in, `admit` sorts them into the same list, and the fold reads that
//!   list. So the winner of a contested lease is decided by the rail's total
//!   order, never by who gossiped first, and a second sort here would be a
//!   second decider for exactly that question (ARCH §10.6).
//! - **A retracted act contributes nothing, wherever its correction sits.**
//!   `admit` builds its void set from every surviving correction at once,
//!   "with no regard for order" (`admit.rs`), so a correction that lands
//!   BEFORE the act it corrects in the total order still voids it. A fold that
//!   walked `Admission::ops` instead of `applied()` would apply the retracted
//!   act, and the node that never received the correction and the node that
//!   did would disagree about who holds a lease — silently, both green.
//!
//! `the_projection_is_a_function_of_the_surviving_acts_not_of_the_walk` pins
//! both halves in one property, and it was watched red with the second half
//! removed before it was trusted (ARCH §18.1).
//!
//! # Time
//!
//! **The core reads no clock.** `now_ms` is a parameter of every function
//! whose answer depends on it, which is what lets a peer replaying this
//! journal in 2029 reproduce the fold exactly. Rail timestamps are Unix
//! *seconds* (`AdmittedOp::ts_unix`); everything downstream of the fold is
//! Unix *milliseconds*, because the two constants it reuses — [`LEASE_MS`] and
//! `UnitStatus::Leased::expires_at_ms` — are already in milliseconds and
//! converting at the one boundary is cheaper than carrying two units.
//!
//! # Expiry is derived, never applied
//!
//! Nothing in this module mutates a projection to reap a lease. A lease past
//! its deadline simply *reads* as queued again — [`ProjectedUnit::status_at`]
//! is the one place that derivation is written — so every node computes the
//! same expiry from the same journal at the same `now_ms` and no node has to
//! publish a reap act for the others to agree (the rule
//! `commonwealth-core`'s retention floor already follows). [`expired`] is the
//! COUNT of what a sweep at `now_ms` would move, in `ReapStats`' shape: a
//! sweep that is reported rather than silent (ARCH §18.3).
//!
//! # What is reused, and the two places it could not be
//!
//! Taken whole from `commonwealth_core::knowledge`: [`HandoffPhase`]
//! (`Merging` included and deliberately unused — see [`WorkHandoff::phase_at`]),
//! [`LEASE_MS`] and [`MAX_UNIT_ATTEMPTS`]. The unit transitions and the
//! `ORDER BY attempts ASC, key ASC` fairness rule are
//! `sovereign-pipeline/src/worklist.rs:14-22,191-249`'s, and
//! [`WorkProjection::takeable_at`] is where the second one is written.
//! `unreadable` is `commonwealth-state`'s `rail_kv::Projection.unreadable`
//! discipline: a line this build cannot USE is counted and reported, never
//! dropped.
//!
//! Two citations in the plan could not be honoured as literal reuse, and both
//! are named rather than quietly diverged from:
//!
//! - **`UnitStatus` (`knowledge.rs:348`) with `peer: NodeId` generalized to
//!   `ActorKey`.** The generalization is a default type parameter on that enum
//!   (`pub enum UnitStatus<P = NodeId>`), which is additive — all 63 existing
//!   references keep resolving to `UnitStatus<NodeId>` — but it is an edit to
//!   `commonwealth-core`, which is outside this crate. [`WorkUnitStatus`] is
//!   that enum's four states with `ActorKey` in the peer position and the
//!   work plane's own outcome fields; it is a **named** duplication, and the
//!   commit that should collapse it is cw-lift 5g, which already renames
//!   `commonwealth-core::WorkUnit -> IngestUnit` in the same file.
//! - **`LeasedUnit::is_live_at` (`knowledge.rs:452`, "the one place the
//!   comparison is written").** It is a method on a struct whose `unit` field
//!   is the closed *ingest* `WorkUnit` enum, so calling it from here would
//!   mean constructing a fake ingest unit to ask a `<`. The comparison is
//!   written once instead, in [`lease_is_live`], with the boundary it inherits
//!   (`now_ms < expires_at_ms`, exclusive) stated there.
//!
//! **No new constants.** Every number this module compares against is
//! [`LEASE_MS`], [`MAX_UNIT_ATTEMPTS`], or a `ttl_secs` the submitter wrote.

use std::collections::BTreeMap;

use commonwealth_core::ids::HandoffId;
use commonwealth_core::knowledge::{HandoffPhase, LEASE_MS, MAX_UNIT_ATTEMPTS};
use commonwealth_rail_core::Admission;
use kernel_types::{ComputeAttribution, Judgement};
use oicp_types::{JobKind, JobUnit, WorkOffer};
use serde_json::Value;

use crate::act::{self, RailWorkAct, UnitRef, WorkAct};
use crate::actor::ActorKey;
use crate::seal;

// -----------------------------------------------------------------
// Time
// -----------------------------------------------------------------

/// A rail timestamp (Unix **seconds**, signed) as the milliseconds everything
/// downstream of the fold speaks.
///
/// Saturating and floored at zero rather than wrapping: `ts_unix` is `i64` and
/// a peer with a badly wrong clock can sign a negative one, which must become
/// "the beginning of time" and not `u64::MAX - n`. An act from before the
/// epoch is a fact about that peer's clock, not a lease that outlives the
/// heat death of the universe.
fn ms_of(ts_unix: i64) -> u64 {
    ts_unix.saturating_mul(1_000).max(0) as u64
}

/// Whether a lease taken to `expires_at_ms` is still live at `now_ms`.
///
/// **The one place this comparison is written in this crate**, for the reason
/// `commonwealth_core::knowledge::LeasedUnit::is_live_at` says at its own site:
/// the two ends must not disagree about whether the boundary is inclusive. It
/// is not, and the deadline second belongs to nobody — `now_ms < expires`, the
/// same direction `is_live_at` writes.
///
/// It is a copy of that method rather than a call to it because `LeasedUnit`
/// carries the ingest `WorkUnit` enum in its `unit` field, so reaching the
/// method would mean minting a fake ingest unit to ask a `<`. See this
/// module's docs.
fn lease_is_live(expires_at_ms: u64, now_ms: u64) -> bool {
    now_ms < expires_at_ms
}

// -----------------------------------------------------------------
// The unit
// -----------------------------------------------------------------

/// Where one unit is in its life.
///
/// `commonwealth_core::knowledge::UnitStatus`'s four states, with the peer
/// spelled as the rail spells it — an [`ActorKey`], the signing key admission
/// verified, never a self-reported node id — and with the work plane's outcome
/// vocabulary attached where ingest carried none. See this module's docs for
/// why it is a copy and which commit collapses it.
///
/// The states are exactly `worklist.rs`'s: `pending -> claimed -> done`, with
/// `claimed -> pending` on a lapse or a retryable failure and
/// `claimed -> failed` once the attempts are spent.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkUnitStatus {
    /// Waiting to be taken. `prior_attempts` is 0 on submission and N after N
    /// leases that did not finish, so [`MAX_UNIT_ATTEMPTS`] counts total
    /// attempts and not per-actor attempts.
    Queued { prior_attempts: u32 },
    /// An actor holds the lease. `Renew` pushes `expires_at_ms` out; past it,
    /// [`ProjectedUnit::status_at`] reads this back as `Queued` (or terminal).
    Leased {
        lessee: ActorKey,
        leased_at_ms: u64,
        last_renewed_ms: u64,
        expires_at_ms: u64,
        /// 1 on the first lease, incremented on every re-lease.
        attempts: u32,
    },
    /// The unit ran and reached a verdict. `outcome` may itself be a FAILED
    /// verdict — a red test shard is a completed unit, and the distinction
    /// between "the work failed" and "the plane failed to run the work" is
    /// the whole reason `Complete` and [`Failed`](WorkUnitStatus::Failed) are
    /// different states.
    Complete {
        lessee: ActorKey,
        completed_at_ms: u64,
        /// How many leases it took to get here. Carried because 5e's merged
        /// table wants it beside the verdict: a unit that passed on its third
        /// attempt is not the same evidence as one that passed on its first.
        attempts: u32,
        outcome: Judgement,
        result: Value,
        provenance: ComputeAttribution,
    },
    /// Terminal without a verdict of its own: the attempts are spent.
    ///
    /// `outcome` is `Some` when the last lessee reported a `Fail` act
    /// (`CouldNotJudge` or `NeverRan`) and `None` when nobody reported at all
    /// — the lease simply lapsed [`MAX_UNIT_ATTEMPTS`] times. Keeping the two
    /// apart is the work atlas's `Expired`-vs-`Free` distinction
    /// (`resource_may_i.rs:65`): a unit that finished badly and a unit nobody
    /// ever finished are both terminal and are not the same fact.
    Failed {
        last_lessee: ActorKey,
        reason: String,
        attempts: u32,
        outcome: Option<Judgement>,
    },
}

impl WorkUnitStatus {
    /// A short, stable discriminator for logs and for `svrn job status`.
    pub fn id(&self) -> &'static str {
        match self {
            WorkUnitStatus::Queued { .. } => "queued",
            WorkUnitStatus::Leased { .. } => "leased",
            WorkUnitStatus::Complete { .. } => "complete",
            WorkUnitStatus::Failed { .. } => "failed",
        }
    }

    /// Whether nothing further will happen to this unit.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkUnitStatus::Complete { .. } | WorkUnitStatus::Failed { .. }
        )
    }
}

/// One unit as the journal leaves it: the submitted unit, and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedUnit {
    /// The unit exactly as its `Submit` carried it, seal verified.
    pub unit: JobUnit,
    /// What the acts say, WITHOUT expiry applied. Read
    /// [`status_at`](Self::status_at) rather than this field unless you mean
    /// "what was written", because a lapsed lease is still written here.
    pub status: WorkUnitStatus,
}

impl ProjectedUnit {
    /// The status at `now_ms`, with lease expiry derived.
    ///
    /// **The one place expiry is decided.** A lease past its deadline reads as
    /// `Queued` with its attempts carried forward, or as terminal `Failed`
    /// once [`MAX_UNIT_ATTEMPTS`] leases have lapsed — `worklist.rs`'s
    /// `ack_failure` rule, applied to a lapse instead of a report. Nothing is
    /// mutated and nothing is published: every node derives the same answer
    /// from the same journal and the same `now_ms`.
    pub fn status_at(&self, now_ms: u64) -> WorkUnitStatus {
        match &self.status {
            WorkUnitStatus::Leased {
                lessee,
                expires_at_ms,
                attempts,
                ..
            } if !lease_is_live(*expires_at_ms, now_ms) => {
                if *attempts >= MAX_UNIT_ATTEMPTS {
                    WorkUnitStatus::Failed {
                        last_lessee: lessee.clone(),
                        reason: format!(
                            "the lease lapsed {attempts} times and nobody reported — \
                             MAX_UNIT_ATTEMPTS is {MAX_UNIT_ATTEMPTS}"
                        ),
                        attempts: *attempts,
                        outcome: None,
                    }
                } else {
                    WorkUnitStatus::Queued {
                        prior_attempts: *attempts,
                    }
                }
            }
            settled => settled.clone(),
        }
    }
}

// -----------------------------------------------------------------
// The handoff
// -----------------------------------------------------------------

/// One submitted handoff and its units.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkHandoff {
    /// Who submitted it — from ADMISSION, never from the payload. Only this
    /// actor may revoke it.
    pub submitter: ActorKey,
    /// The kind every unit in it agrees with (`act::to_payload` refuses a
    /// submission where one does not).
    pub kind: JobKind,
    /// The submitter's half of the grant. `None` open, `Some(list)` those
    /// actors, `Some(∅)` self-only.
    pub allowed: Option<Vec<ActorKey>>,
    /// When the `Submit` was admitted.
    pub submitted_at_ms: u64,
    /// When the handoff stops being offered — `submitted_at_ms` plus the
    /// submission's clamped `ttl_secs`.
    pub expires_at_ms: u64,
    /// Set by a `Revoke` from the submitter. `Some` carries the sentence the
    /// phase renders.
    pub revoked: Option<String>,
    /// Units by `unit_hash`. A `BTreeMap` so iteration is the same on every
    /// node without a second sort.
    pub units: BTreeMap<String, ProjectedUnit>,
}

impl WorkHandoff {
    /// The handoff's phase at `now_ms`, derived from its units.
    ///
    /// [`HandoffPhase`] is taken whole from `commonwealth_core::knowledge` and
    /// **`Merging` is never produced here**. That state means "the leader is
    /// merging peer partitions into one corpus", which is an ingest step: for
    /// a `process:v1` unit there is nothing to merge, the results are already
    /// on the journal as `Complete` acts. It is not pruned from the enum
    /// because pruning it would fork the vocabulary ingest still uses, and
    /// cw-lift 5g brings ingest onto this fold, at which point an
    /// `IngestExecutor` is what will produce it.
    pub fn phase_at(&self, now_ms: u64) -> HandoffPhase {
        if let Some(reason) = &self.revoked {
            return HandoffPhase::Failed {
                reason: reason.clone(),
            };
        }
        let mut queued = 0usize;
        let mut leased = 0usize;
        for unit in self.units.values() {
            match unit.status_at(now_ms) {
                WorkUnitStatus::Queued { .. } => queued += 1,
                WorkUnitStatus::Leased { .. } => leased += 1,
                _ => {}
            }
        }
        if queued == 0 && leased == 0 {
            return HandoffPhase::Complete;
        }
        if now_ms >= self.expires_at_ms {
            return HandoffPhase::Failed {
                reason: format!(
                    "the handoff's ttl elapsed at {}ms with {queued} unit(s) unclaimed and \
                     {leased} still leased",
                    self.expires_at_ms
                ),
            };
        }
        if queued > 0 {
            HandoffPhase::Open
        } else {
            HandoffPhase::Draining
        }
    }

    /// Whether the submitter's half of the grant admits `actor` at `now_ms`.
    ///
    /// Three ways it can be false and they are one fact — the submitter has
    /// not consented to this actor taking this unit *now*: the allow-list does
    /// not name them, the handoff was revoked, or its TTL elapsed.
    pub fn admits(&self, actor: &ActorKey, now_ms: u64) -> bool {
        if self.revoked.is_some() || now_ms >= self.expires_at_ms {
            return false;
        }
        match &self.allowed {
            None => true,
            Some(list) => list.contains(actor),
        }
    }
}

// -----------------------------------------------------------------
// The losers, and the sweep
// -----------------------------------------------------------------

/// A `Lease` act that arrived for a unit somebody else already held.
///
/// **Reported, never dropped** (ARCH §18.3), and this is the reason the fold
/// is modelled on `measurements_rail` rather than `rail_kv`: last-writer-wins
/// has no loser, and here the loser is the point. The losing actor started
/// work it must now cancel, and this row is how it finds out — the fact the
/// HTTP path spells `HeartbeatResult::Reclaimed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostLease {
    pub handoff: HandoffId,
    pub unit_hash: String,
    /// The actor whose lease stands.
    pub winner: ActorKey,
    /// The actor whose lease was ignored — the one that must cancel.
    pub loser: ActorKey,
    /// When the losing act was admitted.
    pub at_ms: u64,
}

/// What a lease sweep at one instant would move.
///
/// `commonwealth-knowledge`'s `work_queue.rs:96` counted shape, re-declared
/// here because that crate is one of the three
/// `commonwealth-{knowledge,inference,api}` applications this package's
/// closure may not reach. `sovereign code converge noun ReapStats` reports the
/// one other definition; the convergence is to lift it to `kernel-types`
/// (converge's own lowest-tier candidate), and cw-lift 5g deletes the
/// `work_queue.rs` copy along with the rest of that lease machinery.
///
/// Counts, not a mutation: [`expired`] tells you what a sweep WOULD move, and
/// every reader derives the same answer from the same journal. An expiry that
/// nobody counted is the silent sweep ARCH §18.3 forbids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapStats {
    /// Lapsed leases whose unit goes back on the queue with `attempts + 1`.
    pub requeued: u32,
    /// Lapsed leases whose unit is terminal — [`MAX_UNIT_ATTEMPTS`] spent.
    pub terminal_failed: u32,
    /// Handoffs whose [`HandoffPhase`] is different because of these lapses.
    pub phase_transitions: u32,
}

/// Count what a lease sweep at `now_ms` would move.
///
/// Derived, never applied: nothing here mutates `proj`, and no node publishes
/// the result. Two nodes folding the same journal and asking at the same
/// `now_ms` get the same counts, which is what lets expiry be a shared fact
/// without a shared reaper.
pub fn expired(proj: &WorkProjection, now_ms: u64) -> ReapStats {
    let mut stats = ReapStats::default();
    for (id, handoff) in &proj.handoffs {
        let mut lapsed_here = false;
        for (hash, unit) in &handoff.units {
            let WorkUnitStatus::Leased {
                expires_at_ms,
                attempts,
                ..
            } = &unit.status
            else {
                continue;
            };
            if lease_is_live(*expires_at_ms, now_ms) {
                continue;
            }
            lapsed_here = true;
            if *attempts >= MAX_UNIT_ATTEMPTS {
                stats.terminal_failed += 1;
            } else {
                stats.requeued += 1;
            }
            tracing::debug!(
                target: crate::TRACE_TARGET,
                handoff = %id,
                unit = %hash,
                attempts = *attempts,
                expires_at_ms = *expires_at_ms,
                now_ms,
                "work lease lapsed"
            );
        }
        if !lapsed_here {
            continue;
        }
        // What the phase WOULD be if none of those leases had lapsed: the
        // deadline of the longest-lived one, minus a millisecond, is the last
        // instant at which they were all still live.
        let last_all_live = handoff
            .units
            .values()
            .filter_map(|u| match &u.status {
                WorkUnitStatus::Leased { expires_at_ms, .. } => Some(*expires_at_ms),
                _ => None,
            })
            .max()
            .map(|deadline| deadline.saturating_sub(1))
            .unwrap_or(now_ms);
        if handoff.phase_at(now_ms) != handoff.phase_at(last_all_live) {
            stats.phase_transitions += 1;
        }
    }
    stats
}

// -----------------------------------------------------------------
// The projection
// -----------------------------------------------------------------

/// The `work` namespace, folded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkProjection {
    /// Every handoff an admitted `Submit` opened, by id.
    pub handoffs: BTreeMap<HandoffId, WorkHandoff>,
    /// The live offer per donor — latest admitted `Offer` wins, which is
    /// simply the last one in the total order.
    pub offers: BTreeMap<ActorKey, WorkOffer>,
    /// Every `Lease` that arrived for a unit somebody already held.
    pub lost_leases: Vec<LostLease>,
    /// `Complete`/`Fail` acts repeating a report the lessee already made.
    ///
    /// Delivery on this plane is at-least-once and idempotent per `unit_hash`,
    /// so a repeat is normal rather than an error — but it is counted, because
    /// a donor reporting the same unit five times is a fact about that donor.
    pub double_deliveries: usize,
    /// Admitted lines this build could not USE — the sum of the ones
    /// `act::read` could not decode and the ones this fold could not apply (a
    /// lease for a unit nobody submitted, a `Complete` from an actor that
    /// never held the lease, a `Revoke` from somebody who is not the
    /// submitter). `rail_kv::Projection.unreadable`'s discipline: counted and
    /// reported, never dropped (ARCH §18.3).
    pub unreadable: usize,
    /// Everything admission could not account for — an unknown signer, a hole
    /// in a peer's sequence, a torn line. An empty projection beside a
    /// non-zero `gaps` is a very different fact from an empty projection
    /// beside a quiet ring.
    pub gaps: usize,
}

impl WorkProjection {
    /// Fold an admission into the queue.
    ///
    /// Pure, and a function of the SURVIVING acts in admission's total order —
    /// see this module's docs for the two halves of that and for the property
    /// that pins them.
    pub fn fold(admission: &Admission) -> WorkProjection {
        let acts = act::read(admission);
        let mut proj = WorkProjection {
            unreadable: acts.unreadable,
            gaps: acts.gaps,
            ..Default::default()
        };
        for line in &acts.found {
            proj.apply(line);
        }
        tracing::debug!(
            target: crate::TRACE_TARGET,
            handoffs = proj.handoffs.len(),
            offers = proj.offers.len(),
            lost_leases = proj.lost_leases.len(),
            double_deliveries = proj.double_deliveries,
            unreadable = proj.unreadable,
            gaps = proj.gaps,
            "work projection folded"
        );
        proj
    }

    /// Every unit that is takeable at `now_ms`, in the fairness order.
    ///
    /// `ORDER BY attempts ASC, key ASC` — `worklist.rs:191-249`'s rule,
    /// verbatim in intent: a unit that has already burnt an attempt goes to
    /// the back, so one poisonous unit cannot monopolise a cohort, and the tie
    /// is broken by a stable key so two nodes reading the same journal offer
    /// the same unit next. The key here is `(handoff, unit_hash)` because the
    /// plane holds many handoffs at once and `unit_hash` alone would order
    /// them by a content hash, which is arbitrary ACROSS handoffs as well as
    /// within one.
    ///
    /// This is the queue only — it says nothing about whether YOU may take a
    /// unit. That is [`crate::refusal::may_take`], and it is the one predicate.
    pub fn takeable_at(&self, now_ms: u64) -> Vec<UnitRef> {
        let mut rows: Vec<(u32, &HandoffId, &String)> = Vec::new();
        for (id, handoff) in &self.handoffs {
            if handoff.revoked.is_some() || now_ms >= handoff.expires_at_ms {
                continue;
            }
            for (hash, unit) in &handoff.units {
                if let WorkUnitStatus::Queued { prior_attempts } = unit.status_at(now_ms) {
                    rows.push((prior_attempts, id, hash));
                }
            }
        }
        rows.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        rows.into_iter()
            .map(|(_, handoff, hash)| UnitRef {
                handoff: *handoff,
                unit_hash: hash.clone(),
            })
            .collect()
    }

    /// The unit a [`UnitRef`] names, if this journal carries it.
    pub fn unit(&self, r: &UnitRef) -> Option<&ProjectedUnit> {
        self.handoffs.get(&r.handoff)?.units.get(&r.unit_hash)
    }

    /// How many live leases `actor` holds at `now_ms`, across every handoff —
    /// what `WorkOffer::max_concurrent` is compared against.
    pub fn leases_held_by(&self, actor: &ActorKey, now_ms: u64) -> u32 {
        let mut held = 0u32;
        for handoff in self.handoffs.values() {
            for unit in handoff.units.values() {
                if let WorkUnitStatus::Leased { lessee, .. } = unit.status_at(now_ms) {
                    if &lessee == actor {
                        held += 1;
                    }
                }
            }
        }
        held
    }

    // ── one act ──────────────────────────────────────────────

    fn apply(&mut self, line: &RailWorkAct) {
        let at_ms = ms_of(line.ts_unix);
        match &line.act {
            WorkAct::Submit(s) => {
                if self.handoffs.contains_key(&s.handoff) {
                    // First `Submit` for a handoff id wins. A second one is
                    // either a replay the rail already deduplicated by op id
                    // (so this is a DIFFERENT act claiming the same handoff)
                    // or another actor reopening somebody's handoff, and
                    // neither may silently rewrite the units under a donor
                    // that is already running them.
                    return self.unreadable(line, "a handoff with this id is already open");
                }
                let mut units = BTreeMap::new();
                for unit in &s.units {
                    // The rail door verifies this too. It is checked again
                    // because a peer on a different build may have appended
                    // through another door, and a unit whose hash does not
                    // cover its payload cannot be leased or completed
                    // idempotently — which is the whole job of the hash.
                    if let Err(e) = seal::verify(unit) {
                        self.unreadable(line, &e.to_string());
                        continue;
                    }
                    units.insert(
                        unit.unit_hash.clone(),
                        ProjectedUnit {
                            unit: unit.clone(),
                            status: WorkUnitStatus::Queued { prior_attempts: 0 },
                        },
                    );
                }
                self.handoffs.insert(
                    s.handoff,
                    WorkHandoff {
                        submitter: line.actor.clone(),
                        kind: s.kind.clone(),
                        allowed: s.allowed.clone(),
                        submitted_at_ms: at_ms,
                        expires_at_ms: at_ms.saturating_add(s.ttl_secs.saturating_mul(1_000)),
                        revoked: None,
                        units,
                    },
                );
            }
            WorkAct::Offer(offer) => {
                // Latest per actor wins, and "latest" is simply "last in the
                // total order" — no second comparison, no timestamp read off
                // the payload.
                self.offers.insert(line.actor.clone(), offer.clone());
            }
            WorkAct::Lease(r) => self.lease(line, r, at_ms),
            WorkAct::Renew(r) => self.renew(line, r, at_ms),
            WorkAct::Complete(c) => self.complete(
                line,
                &UnitRef {
                    handoff: c.handoff,
                    unit_hash: c.unit_hash.clone(),
                },
                at_ms,
                c.outcome.clone(),
                Some((c.result.clone(), c.provenance.clone())),
            ),
            WorkAct::Fail(f) => self.complete(
                line,
                &UnitRef {
                    handoff: f.handoff,
                    unit_hash: f.unit_hash.clone(),
                },
                at_ms,
                f.outcome.clone(),
                None,
            ),
            WorkAct::Revoke(r) => {
                let Some(handoff) = self.handoffs.get_mut(&r.handoff) else {
                    return self.unreadable(line, "no admitted submission opened this handoff");
                };
                if handoff.submitter != line.actor {
                    return self.unreadable(line, "only the submitter of a handoff may revoke it");
                }
                handoff.revoked = Some(format!("revoked by its submitter at {at_ms}ms"));
            }
        }
    }

    fn lease(&mut self, line: &RailWorkAct, r: &UnitRef, at_ms: u64) {
        let Some(handoff) = self.handoffs.get_mut(&r.handoff) else {
            return self.unreadable(line, "no admitted submission opened this handoff");
        };
        let Some(unit) = handoff.units.get_mut(&r.unit_hash) else {
            return self.unreadable(line, "this handoff carries no unit with that hash");
        };
        match unit.status_at(at_ms) {
            WorkUnitStatus::Queued { prior_attempts } => {
                unit.status = WorkUnitStatus::Leased {
                    lessee: line.actor.clone(),
                    leased_at_ms: at_ms,
                    last_renewed_ms: at_ms,
                    expires_at_ms: at_ms.saturating_add(LEASE_MS),
                    attempts: prior_attempts.saturating_add(1),
                };
            }
            WorkUnitStatus::Leased { lessee, .. } => {
                // The second lease on a held unit is IGNORED and REPORTED.
                // The loser has already started work and must cancel; a fold
                // that quietly dropped this act would leave it running.
                let lost = LostLease {
                    handoff: r.handoff,
                    unit_hash: r.unit_hash.clone(),
                    winner: lessee,
                    loser: line.actor.clone(),
                    at_ms,
                };
                tracing::debug!(
                    target: crate::TRACE_TARGET,
                    handoff = %lost.handoff,
                    unit = %lost.unit_hash,
                    winner = %lost.winner,
                    loser = %lost.loser,
                    "work lease lost"
                );
                self.lost_leases.push(lost);
            }
            terminal => {
                self.unreadable(
                    line,
                    &format!(
                        "this unit is already {} and will not be offered again",
                        terminal.id()
                    ),
                );
            }
        }
    }

    fn renew(&mut self, line: &RailWorkAct, r: &UnitRef, at_ms: u64) {
        let Some(handoff) = self.handoffs.get_mut(&r.handoff) else {
            return self.unreadable(line, "no admitted submission opened this handoff");
        };
        let Some(unit) = handoff.units.get_mut(&r.unit_hash) else {
            return self.unreadable(line, "this handoff carries no unit with that hash");
        };
        // Read the WRITTEN status, not the derived one: a renew that arrives
        // after the deadline is a different fact from one that arrives for a
        // unit the actor never held, and both are reported rather than
        // silently extending a lease the rest of the ring has already
        // re-offered.
        let WorkUnitStatus::Leased {
            lessee,
            leased_at_ms,
            expires_at_ms,
            attempts,
            ..
        } = unit.status.clone()
        else {
            return self.unreadable(line, "this unit is not leased");
        };
        if lessee != line.actor {
            return self.unreadable(line, "a renew from an actor that does not hold the lease");
        }
        if !lease_is_live(expires_at_ms, at_ms) {
            return self.unreadable(
                line,
                "the lease had already lapsed when this renew was admitted — every other node \
                 has re-offered the unit, and extending it here would be the one node \
                 disagreeing",
            );
        }
        unit.status = WorkUnitStatus::Leased {
            lessee,
            leased_at_ms,
            last_renewed_ms: at_ms,
            expires_at_ms: at_ms.saturating_add(LEASE_MS),
            attempts,
        };
    }

    /// `Complete` and `Fail` are one path: both are the lessee reporting, and
    /// the only difference is whether there is a result to carry. A second
    /// implementation of "is this actor the lessee" is exactly the kind of
    /// second decider ARCH §10.6 is about.
    fn complete(
        &mut self,
        line: &RailWorkAct,
        r: &UnitRef,
        at_ms: u64,
        outcome: Judgement,
        result: Option<(Value, ComputeAttribution)>,
    ) {
        let Some(handoff) = self.handoffs.get_mut(&r.handoff) else {
            return self.unreadable(line, "no admitted submission opened this handoff");
        };
        let Some(unit) = handoff.units.get_mut(&r.unit_hash) else {
            return self.unreadable(line, "this handoff carries no unit with that hash");
        };
        match &unit.status {
            WorkUnitStatus::Leased {
                lessee, attempts, ..
            } => {
                if lessee != &line.actor {
                    // The named refusal: a report from an actor that never
                    // held the lease. Counted, never applied — accepting it
                    // would let any ring member write a verdict for work
                    // somebody else was doing.
                    return self.unreadable(
                        line,
                        "a report from an actor that does not hold this unit's lease",
                    );
                }
                let attempts = *attempts;
                unit.status = match result {
                    Some((result, provenance)) => WorkUnitStatus::Complete {
                        lessee: line.actor.clone(),
                        completed_at_ms: at_ms,
                        attempts,
                        outcome,
                        result,
                        provenance,
                    },
                    // `worklist.rs`'s `ack_failure`: back on the queue while
                    // attempts remain, terminal once they are spent. The
                    // attempt is already counted — the lease incremented it.
                    None if attempts < MAX_UNIT_ATTEMPTS => WorkUnitStatus::Queued {
                        prior_attempts: attempts,
                    },
                    None => WorkUnitStatus::Failed {
                        last_lessee: line.actor.clone(),
                        reason: outcome.reason().as_str().to_string(),
                        attempts,
                        outcome: Some(outcome),
                    },
                };
            }
            settled if settled.is_terminal() => {
                // At-least-once delivery: a repeat is normal, and counted.
                self.double_deliveries += 1;
                tracing::debug!(
                    target: crate::TRACE_TARGET,
                    handoff = %r.handoff,
                    unit = %r.unit_hash,
                    actor = %line.actor,
                    "work report for a unit already settled"
                );
            }
            _ => {
                self.unreadable(line, "a report for a unit nobody holds a lease on");
            }
        }
    }

    fn unreadable(&mut self, line: &RailWorkAct, why: &str) {
        tracing::debug!(
            target: crate::TRACE_TARGET,
            actor = %line.actor,
            seq = line.seq,
            act = %line.act.kind(),
            why,
            "work act unusable"
        );
        self.unreadable += 1;
    }
}

/// Fixtures shared by this crate's Wave-2 test modules.
///
/// ONE ring and one set of builders, for the reason `commonwealth-rail-core`'s
/// own `tests_support` gives at its site: two copies of "what does a signed
/// work act look like" would be two answers to the question the signature
/// exists to settle (ARCH §10.6), and the fold's tests and the predicate's
/// tests have to be talking about the same op or neither proves anything.
///
/// It is not the rail's `tests_support` itself, for two reasons that are both
/// facts and not preferences: that module builds `{"kind":"thing"}` payloads,
/// which this fold refuses by design, and reaching it at all needs a
/// `test-support` feature on a dependency declared in a manifest this lane
/// does not own. Everything below is built through the rail's ordinary public
/// surface.
#[cfg(test)]
pub(crate) mod tests_fixture {
    use super::*;
    use crate::act::{Completion, Failure, Submission};
    use crate::WORK_NAMESPACE;
    use commonwealth_rail_core::{
        actor_of, admit, body_json, sign_ring_op, Ed25519Verifier, Op, Person, RailAct, Roster,
        SignedOp, SigningKey,
    };
    use kernel_types::judgement::Reason;
    use kernel_types::{NodeId, Server};
    use oicp_types::{Isolation, JobRequirements};
    use serde_json::json;
    use std::collections::BTreeMap as Map;

    // ── the ring ─────────────────────────────────────────────

    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    /// The actor key seed `seed` signs with. 1 is alex, 2 bo, 3 cy.
    pub fn who(seed: u8) -> ActorKey {
        ActorKey::parse(actor_of(&key(seed))).expect("actor_of emits canonical hex")
    }

    pub fn ring() -> Roster {
        let mut m = Map::new();
        m.insert(Person::from("alex"), vec![actor_of(&key(1))]);
        m.insert(Person::from("bo"), vec![actor_of(&key(2))]);
        m.insert(Person::from("cy"), vec![actor_of(&key(3))]);
        Roster::new(m)
    }

    pub fn signed(seed: u8, ts: i64, seq: u64, act: RailAct) -> Op<SignedOp> {
        let k = key(seed);
        let sig = sign_ring_op(&k, WORK_NAMESPACE, ts, seq, &body_json(&act));
        Op::new(SignedOp { seq, sig, act }, ts, actor_of(&k))
    }

    /// One work act, signed onto the rail by `seed` at second `ts`.
    pub fn op(seed: u8, ts: i64, seq: u64, act: &WorkAct) -> Op<SignedOp> {
        signed(
            seed,
            ts,
            seq,
            RailAct::Record {
                payload: act::to_payload(act).expect("a well-formed act"),
            },
        )
    }

    /// A correction that voids `target` and states nothing in its place.
    pub fn correct(seed: u8, ts: i64, seq: u64, target: &Op<SignedOp>) -> Op<SignedOp> {
        signed(
            seed,
            ts,
            seq,
            RailAct::Correct {
                corrects: target.id.clone(),
                replacement: None,
            },
        )
    }

    pub fn fold(ops: &[Op<SignedOp>]) -> WorkProjection {
        WorkProjection::fold(&admit(ops, &[], &ring(), WORK_NAMESPACE, &Ed25519Verifier))
    }

    // ── the work ─────────────────────────────────────────────

    pub fn kind(raw: &str) -> JobKind {
        JobKind::parse(raw).expect("test kind")
    }

    pub fn unit_of(k: &str, body: Value) -> JobUnit {
        seal::seal(kind(k), body, JobRequirements::any(), None).expect("sealed")
    }

    pub fn handoff() -> HandoffId {
        HandoffId::from_u128(7)
    }

    pub fn offer(kinds: &[&str], accept_from: Option<Vec<String>>) -> WorkOffer {
        WorkOffer {
            kinds: kinds.iter().map(|k| kind(k)).collect(),
            max_concurrent: 2,
            yield_to_foreground: false,
            isolation: Isolation::Subprocess,
            os: "linux".into(),
            arch: "x86_64".into(),
            repos: vec![],
            accept_from,
        }
    }

    pub fn provenance() -> ComputeAttribution {
        ComputeAttribution {
            repo_rev: "5c98898c7".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            toolchain: "rustc 1.90.0".into(),
            host: Server::Peer {
                node: NodeId::from_u128(3),
                name: "beefymac".into(),
            },
        }
    }

    /// One handoff, two units, open to the ring, submitted by alex.
    pub fn submission() -> (WorkAct, JobUnit, JobUnit) {
        let a = unit_of("process:v1", json!({ "argv": ["uname", "-a"] }));
        let b = unit_of("process:v1", json!({ "argv": ["true"] }));
        (
            WorkAct::Submit(Submission::new(
                handoff(),
                kind("process:v1"),
                vec![a.clone(), b.clone()],
                None,
                None,
            )),
            a,
            b,
        )
    }

    pub fn unit_ref(unit: &JobUnit) -> UnitRef {
        UnitRef {
            handoff: handoff(),
            unit_hash: unit.unit_hash.clone(),
        }
    }

    pub fn lease(unit: &JobUnit) -> WorkAct {
        WorkAct::Lease(unit_ref(unit))
    }

    pub fn renew(unit: &JobUnit) -> WorkAct {
        WorkAct::Renew(unit_ref(unit))
    }

    pub fn completion(unit: &JobUnit) -> WorkAct {
        WorkAct::Complete(Completion {
            handoff: handoff(),
            unit_hash: unit.unit_hash.clone(),
            outcome: Judgement::passed("unit", Reason::literal("8412 passed, 0 failed")),
            result: json!({ "exit_code": 0 }),
            provenance: provenance(),
        })
    }

    pub fn failure(unit: &JobUnit) -> WorkAct {
        WorkAct::Fail(Failure {
            handoff: handoff(),
            unit_hash: unit.unit_hash.clone(),
            outcome: Judgement::could_not_judge("unit", Reason::literal("killed on timeout")),
            provenance: provenance(),
        })
    }

    pub fn status(proj: &WorkProjection, unit: &JobUnit, now_ms: u64) -> WorkUnitStatus {
        proj.handoffs[&handoff()].units[&unit.unit_hash].status_at(now_ms)
    }
}

#[cfg(test)]
mod tests;
