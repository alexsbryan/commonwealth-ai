// SPDX-License-Identifier: AGPL-3.0-or-later
//! The folded `work` queue's types — what `cw-rails`' `GET
//! /v1/work/projection` serves and a submitter or donor reads. The fold
//! itself (`commonwealth_work::projection::fold`) and the handoff's phase
//! (`commonwealth_work::projection::phase_at`, which names
//! `commonwealth-core`'s `HandoffPhase`) stay with the rail (pb-work-doors).

use std::collections::BTreeMap;

use kernel_types::{ActorKey, ComputeAttribution, HandoffId, Judgement};
use serde_json::Value;

use super::act::UnitRef;
use super::wire;
use super::MAX_UNIT_ATTEMPTS;
use crate::{JobKind, JobUnit, WorkOffer};

/// Whether a lease taken to `expires_at_ms` is still live at `now_ms`.
///
/// **The one place this comparison is written for the work plane**, for the reason
/// `commonwealth_core::knowledge::LeasedUnit::is_live_at` says at its own site:
/// the two ends must not disagree about whether the boundary is inclusive. It
/// is not, and the deadline second belongs to nobody — `now_ms < expires`, the
/// same direction `is_live_at` writes.
///
/// It is a copy of that method rather than a call to it because `LeasedUnit`
/// carries the ingest `WorkUnit` enum in its `unit` field, so reaching the
/// method would mean minting a fake ingest unit to ask a `<`. See this
/// module's docs.
pub fn lease_is_live(expires_at_ms: u64, now_ms: u64) -> bool {
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
/// Counts, not a mutation: `commonwealth_work::projection::expired` tells you what a sweep WOULD move, and
/// every reader derives the same answer from the same journal. An expiry that
/// nobody counted is the silent sweep ARCH §18.3 forbids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapStats {
    /// Lapsed leases whose unit goes back on the queue with `attempts + 1`.
    pub requeued: u32,
    /// Lapsed leases whose unit is terminal — [`MAX_UNIT_ATTEMPTS`] spent.
    pub terminal_failed: u32,
    /// Handoffs whose `HandoffPhase` is different because of these lapses.
    pub phase_transitions: u32,
}

/// Does the holder still hold it — and the third answer, which is the point.
///
/// **Three states, not two.** A journal that could not be READ is not evidence
/// that somebody else took the lease, and cancelling a half-hour shard on it
/// is the substitution ARCH §18.3 forbids. [`Unknown`](LeaseState::Unknown) is
/// that verdict, kept apart from [`Lost`](LeaseState::Lost) so a caller can
/// HOLD rather than kill.
///
/// This is not a hypothetical. cw-lift 5f wrote a second donor — a lifted peer
/// — and the first version of it collapsed this to a bool, so a single
/// unreadable heartbeat cancelled a running unit. The decider existed at the
/// time, in `sovereign-daemon::work_donor`, where a package consumer could not
/// reach it; 5f recorded that as a hole in this crate's surface and the peer's
/// own comment named the repair. This is it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    /// The fold says this actor holds the lease at `now_ms`.
    Held,
    /// The fold is readable and says the lease is NOT this actor's — taken by
    /// another donor, lapsed past `expires_at_ms`, or already reported. The
    /// string names which, for the log line the caller writes.
    Lost(String),
    /// The fold could not be read. Says nothing about the lease.
    ///
    /// `commonwealth_work::projection::lease_state` never returns this — it is handed a projection, so by
    /// construction it HAS one. It is constructed by the caller that failed to
    /// obtain the fold, which is the only place that knows.
    Unknown,
}
// -----------------------------------------------------------------
// The projection
// -----------------------------------------------------------------

/// The `work` namespace, folded.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkProjection {
    /// Every handoff an admitted `Submit` opened, by id.
    #[serde(with = "wire::handoffs_as_pairs")]
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
    /// unit. That is `commonwealth_work::refusal::may_take`, and it is the one predicate.
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
}
