// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`WorkAct`] — the `work` namespace's journal vocabulary. Its codec over
//! rail-core's `Payload` (`to_payload`, `from_payload`, `kind_of`, `read`)
//! stays in `commonwealth-work`, which re-exports everything here at
//! `commonwealth_work::act` (pb-work-doors).

use kernel_types::{ActorKey, ComputeAttribution, HandoffId, Judgement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{JobKind, JobUnit, WorkOffer};

/// The shortest TTL a submission can carry. A zero-second handoff is a
/// submission nobody can ever lease, which is a silent no-op rather than an
/// answer.
///
/// The bounds are `sovereign-work-atlas`'s `clamp_ttl`
/// (`config.rs:64`, `r.clamp(1, max_ttl_seconds)`) and its shipped defaults,
/// copied rather than depended on: that crate is a `sovereign-*` and this
/// package's closure may not reach it. Same RULE, same numbers, named here so
/// the copy is visible.
pub const MIN_TTL_SECS: u64 = 1;

/// The longest TTL a submission can carry (24h), matching the work atlas's
/// `max_ttl_seconds`.
pub const MAX_TTL_SECS: u64 = 86_400;

/// The TTL a submission that does not say gets (4h), matching the work
/// atlas's `default_ttl_seconds`.
pub const DEFAULT_TTL_SECS: u64 = 14_400;

// -----------------------------------------------------------------
// The acts
// -----------------------------------------------------------------

/// One act on the `work` namespace.
///
/// Variants are verbs and their payloads are nouns, so a fold reads as
/// `WorkAct::Lease(unit)` rather than as seven bags of loose fields. `Lease`
/// and `Renew` share [`UnitRef`] because they say the same thing about the
/// same unit — a second identical struct would be two spellings of one shape.
///
/// The wire form is internally tagged on `kind`, so the discriminator sits at
/// the top level of the payload object where `commonwealth_work::kind_of` can read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorkAct {
    /// Work exists, and here is who may take it.
    #[serde(rename = "work.submit")]
    Submit(Submission),
    /// This node will run these kinds, under these conditions. Latest per
    /// actor wins.
    #[serde(rename = "work.offer")]
    Offer(WorkOffer),
    /// This actor is taking this unit. A second one on a held unit loses, and
    /// the loss is reported.
    #[serde(rename = "work.lease")]
    Lease(UnitRef),
    /// Still running. Pushes the lease's expiry out by the fold's window.
    #[serde(rename = "work.renew")]
    Renew(UnitRef),
    /// The unit ran to a verdict. `outcome` may be a FAILED verdict — a test
    /// shard that ran and went red is a completion, not a failure of the
    /// plane.
    #[serde(rename = "work.complete")]
    Complete(Completion),
    /// The unit did not reach a verdict: the executor errored, the process was
    /// killed, the donor gave up. `outcome` is `CouldNotJudge` or `NeverRan`,
    /// never `Failed` — see [`Failure`].
    #[serde(rename = "work.fail")]
    Fail(Failure),
    /// The submitter withdraws the whole handoff. Units not yet completed stop
    /// being offered.
    #[serde(rename = "work.revoke")]
    Revoke(Revocation),
}

impl WorkAct {
    /// What this act is, without going near its body.
    pub fn kind(&self) -> WorkActKind {
        match self {
            WorkAct::Submit(_) => WorkActKind::Submit,
            WorkAct::Offer(_) => WorkActKind::Offer,
            WorkAct::Lease(_) => WorkActKind::Lease,
            WorkAct::Renew(_) => WorkActKind::Renew,
            WorkAct::Complete(_) => WorkActKind::Complete,
            WorkAct::Fail(_) => WorkActKind::Fail,
            WorkAct::Revoke(_) => WorkActKind::Revoke,
        }
    }

    /// The handoff every act names. There is no act on this plane that is not
    /// about a handoff except [`WorkAct::Offer`], which is about a node.
    pub fn handoff(&self) -> Option<&HandoffId> {
        match self {
            WorkAct::Submit(s) => Some(&s.handoff),
            WorkAct::Offer(_) => None,
            WorkAct::Lease(u) | WorkAct::Renew(u) => Some(&u.handoff),
            WorkAct::Complete(c) => Some(&c.handoff),
            WorkAct::Fail(f) => Some(&f.handoff),
            WorkAct::Revoke(r) => Some(&r.handoff),
        }
    }
}

/// Work exists. The unit of submission is a handoff, not a unit: units in one
/// `Submit` share a kind, an allow-list and a TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    /// The handoff these units belong to.
    ///
    /// Reuses `commonwealth_core::ids::HandoffId` rather than minting a
    /// content-derived second one. The plan's §7.5 line asked for a content
    /// hash, and the reuse wins the tie: `HandoffQueue`, `HandoffPhase` and
    /// every ingest surface are already keyed on THIS type, and a second
    /// `HandoffId` would be one name with two implementations (§10.6) at the
    /// exact seam where 5g merges ingest onto this fold. The property §7.5
    /// actually protects still holds — 128 random bits are neither a counter
    /// nor an address.
    pub handoff: HandoffId,
    /// What kind of work this handoff is. Every unit must agree with it; the
    /// codec refuses a submission where one does not, because an offer is
    /// matched against THIS field and a disagreeing unit would be leased by a
    /// donor that never offered its kind.
    ///
    /// **Spelled `job_kind` on the wire, and it has to be.** The payload's
    /// top-level `kind` is the ACT kind (`work.submit`), which is what
    /// `commonwealth_work::kind_of` reads without decoding; a second field serialising to the
    /// same key would overwrite the discriminator and make every submission
    /// unreadable to a renderer walking the journal. That is not a
    /// hypothetical — it is what this field did until the round-trip test
    /// caught it.
    #[serde(rename = "job_kind")]
    pub kind: JobKind,
    /// The units, each already sealed by `commonwealth_work::seal::seal`.
    pub units: Vec<JobUnit>,
    /// Who may take these units. Tri-state, and all three states mean
    /// something different — the shape `HandoffQueue.allowed_peers` already
    /// has: `None` is open to the ring, `Some(list)` is those actors,
    /// `Some(vec![])` is self-only (a submission that is deliberately not
    /// offered out).
    pub allowed: Option<Vec<ActorKey>>,
    /// How long the handoff stays live, clamped into
    /// `[MIN_TTL_SECS, MAX_TTL_SECS]` by [`Submission::new`].
    pub ttl_secs: u64,
}

impl Submission {
    /// Build a submission, clamping `ttl_secs` the way the work atlas's
    /// `clamp_ttl` does. `None` takes [`DEFAULT_TTL_SECS`].
    ///
    /// Clamped rather than refused because a TTL is a hint about how long to
    /// keep offering work, not a claim about the world: an out-of-range one
    /// has an obvious right answer, which is what separates it from a
    /// fractional `timeout_secs` (that one is refused, in `commonwealth_work::seal`).
    pub fn new(
        handoff: HandoffId,
        kind: JobKind,
        units: Vec<JobUnit>,
        allowed: Option<Vec<ActorKey>>,
        ttl_secs: Option<u64>,
    ) -> Submission {
        Submission {
            handoff,
            kind,
            units,
            allowed,
            ttl_secs: ttl_secs
                .unwrap_or(DEFAULT_TTL_SECS)
                .clamp(MIN_TTL_SECS, MAX_TTL_SECS),
        }
    }

    /// Whether `actor` is inside this submission's half of the grant.
    ///
    /// The grant is `Submit.allowed` ∩ `Offer.accept_from`; this is the
    /// submitter's half, and `WorkOffer::accepts_from` is the donor's. There
    /// is no `Grant` noun because the intersection of two existing fields is
    /// not a third thing.
    pub fn allows(&self, actor: &ActorKey) -> bool {
        match &self.allowed {
            None => true,
            Some(list) => list.contains(actor),
        }
    }
}

/// Which unit, in which handoff. The body of both [`WorkAct::Lease`] and
/// [`WorkAct::Renew`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRef {
    pub handoff: HandoffId,
    /// The unit's identity — `JobUnit::unit_hash`, as `commonwealth_work::seal` computes
    /// it. Kept as the leaf spells it (lowercase hex `String`) rather than
    /// wrapped, because a newtype here would be a second spelling of the field
    /// it is copied from.
    pub unit_hash: String,
}

/// The unit ran and reached a verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completion {
    pub handoff: HandoffId,
    pub unit_hash: String,
    /// The verdict, in the workspace's ONE outcome vocabulary. A red test
    /// shard is `Verdict::Failed` here and still a `Complete` act — the unit
    /// did its job.
    pub outcome: Judgement,
    /// What the unit produced. Capped by the rail itself at
    /// `MAX_PAYLOAD_BYTES`, and refused rather than truncated when it does not
    /// fit — a truncated result is a result nobody can trust.
    pub result: Value,
    /// What machine, rev, arch and toolchain produced it. This is what makes
    /// `ComputeAttribution::comparable_to` able to say "that verdict is not
    /// about your tree".
    pub provenance: ComputeAttribution,
}

/// The unit did not reach a verdict.
///
/// The split from [`Completion`] is the one `JobExecutor::execute`'s signature
/// already draws: `Ok((Judgement, Value))` is a completion, `Err(JobError)` is
/// a failure. So a failure carries no `result` — there is none — and its
/// `outcome` is `CouldNotJudge` or `NeverRan`, which is exactly what the
/// quality runner's own exit-code map produces for a signal death.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub handoff: HandoffId,
    pub unit_hash: String,
    pub outcome: Judgement,
    pub provenance: ComputeAttribution,
}

/// The submitter withdraws a handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub handoff: HandoffId,
}

// -----------------------------------------------------------------
// The discriminator
// -----------------------------------------------------------------

/// What a `work` journal line says it is, readable without decoding the act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkActKind {
    Submit,
    Offer,
    Lease,
    Renew,
    Complete,
    Fail,
    Revoke,
}

impl WorkActKind {
    /// Every kind, so a reader can enumerate the vocabulary without a match.
    pub const ALL: [WorkActKind; 7] = [
        WorkActKind::Submit,
        WorkActKind::Offer,
        WorkActKind::Lease,
        WorkActKind::Renew,
        WorkActKind::Complete,
        WorkActKind::Fail,
        WorkActKind::Revoke,
    ];

    /// The wire spelling. Namespaced (`work.`) because a rail namespace may
    /// one day carry a second app's acts, and an unprefixed `submit` would be
    /// a name anyone could collide with.
    pub const fn as_str(&self) -> &'static str {
        match self {
            WorkActKind::Submit => "work.submit",
            WorkActKind::Offer => "work.offer",
            WorkActKind::Lease => "work.lease",
            WorkActKind::Renew => "work.renew",
            WorkActKind::Complete => "work.complete",
            WorkActKind::Fail => "work.fail",
            WorkActKind::Revoke => "work.revoke",
        }
    }

    /// Read the wire spelling. `None` for anything else — including a line a
    /// newer peer wrote, which is a fact to count, not to fail on.
    pub fn parse(raw: &str) -> Option<WorkActKind> {
        WorkActKind::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

impl std::fmt::Display for WorkActKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
