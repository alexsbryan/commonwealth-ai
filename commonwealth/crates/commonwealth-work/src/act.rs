// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`WorkAct`] — the `work` namespace's journal vocabulary, and its codec.
//!
//! Seven acts, closed (ARCH §2.1). Everything the plane does is one of them
//! signed onto the rail; there is no side channel, no queue server and no
//! lease table.
//!
//! # The codec shape, and which one it was copied from
//!
//! This is the **third** rail fold in the tree and the **second** non-KV one,
//! and it is copied from the non-KV one: `sovereign-mesh/src/
//! measurements_rail.rs:60-152` — `to_payload -> Result<_, String>`,
//! `from_payload -> Option<_>`, `read(&Admission)`, a `kind` discriminator
//! readable without decoding, and a count of the lines this build could not
//! read rather than a silent drop.
//!
//! `commonwealth-state/src/rail_kv.rs` was deliberately NOT the model.
//! `rail_kv` is a last-writer-wins key/value store, and LWW has no loser: a
//! second write simply replaces the first and nothing anywhere records that
//! two writers raced. On this plane the loser is the whole point — a second
//! `Lease` on a held unit is the fact a donor must learn so it can cancel the
//! work it just started. Copying `rail_kv` would have dragged that semantics
//! in for free and it is wrong here. What IS taken from `rail_kv` is its
//! `unreadable` discipline: a line this build cannot read is counted and
//! reported, never dropped (ARCH §18.3).
//!
//! # Two things called `kind`
//!
//! - The **act** kind — [`WorkActKind`], spelled `work.submit`, `work.lease`,
//!   … — is the `kind` key on the rail payload. [`kind_of`] reads it without
//!   decoding the rest, which is what lets `svrn ring log` and a future
//!   second reader tell acts apart cheaply.
//! - The **job** kind — [`JobKind`], spelled `process:v1` — is what work this
//!   is, and it is what [`crate::seal`] hashes into a unit's identity.
//!
//! # Ordering
//!
//! [`read`] returns acts in **admission order and does not sort them**. That
//! is not an oversight to fix later: `Admission::ops` is the total order
//! `(ts_unix, actor, id)` that every node on the ring already agrees on, and a
//! second ordering in this crate would be a second decider for who won a lease
//! (ARCH §10.6). `measurements_rail::read` does sort — by the record's own
//! `measured_at`, for display — and that is exactly the line not to copy.

use commonwealth_core::ids::HandoffId;
use commonwealth_rail_core::{Admission, OpId, Payload, Person};
use kernel_types::{ComputeAttribution, Judgement};
use oicp_types::{JobKind, JobUnit, WorkOffer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::actor::ActorKey;
use crate::seal;

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
/// the top level of the payload object where [`kind_of`] can read it.
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
    /// [`kind_of`] reads without decoding; a second field serialising to the
    /// same key would overwrite the discriminator and make every submission
    /// unreadable to a renderer walking the journal. That is not a
    /// hypothetical — it is what this field did until the round-trip test
    /// caught it.
    #[serde(rename = "job_kind")]
    pub kind: JobKind,
    /// The units, each already sealed by [`crate::seal::seal`].
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
    /// fractional `timeout_secs` (that one is refused, in [`crate::seal`]).
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
    /// The unit's identity — `JobUnit::unit_hash`, as [`crate::seal`] computes
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

// -----------------------------------------------------------------
// The codec
// -----------------------------------------------------------------

/// Wrap one act as a rail payload, or say in a sentence why it cannot travel.
///
/// The refusals are sentences for the reason `measurements_rail`'s are: they
/// reach an operator through the rail's `refused` field and through
/// `svrn job submit`, and "the payload was rejected" is not something a person
/// can act on.
///
/// Two checks happen here and nowhere else, because this is the one door onto
/// the rail:
///
/// 1. **Every unit's seal is verified.** A unit whose hash does not cover its
///    payload can never be deduplicated or completed idempotently, so it must
///    not get onto a journal every peer keeps forever.
/// 2. **Every unit's kind matches the submission's.** An offer is matched
///    against `Submission::kind`; a unit disagreeing with it would be leased
///    by a donor that never offered its kind.
pub fn to_payload(act: &WorkAct) -> Result<Payload, String> {
    if let WorkAct::Submit(submission) = act {
        if submission.units.is_empty() {
            return Err("a submission with no units offers nothing to take".to_string());
        }
        for unit in &submission.units {
            if unit.kind != submission.kind {
                return Err(format!(
                    "unit {} is `{}` but this handoff submits `{}` — an offer is matched \
                     against the handoff's kind, so a unit of another kind would be leased \
                     by a donor that never offered it",
                    unit.unit_hash, unit.kind, submission.kind
                ));
            }
            seal::verify(unit).map_err(|e| e.to_string())?;
        }
    }
    let value = serde_json::to_value(act)
        .map_err(|e| format!("this act could not be encoded as a journal line: {e}"))?;
    Payload::new(value).map_err(|e| e.to_string())
}

/// Read an act back off a journal line, or `None` if this line is not one this
/// build can read.
///
/// Never fails loudly, for the reason `measurements_rail::from_payload` does
/// not: a peer on a newer act vocabulary must not cost this reader every other
/// peer's acts. The count of these is reported to the caller as
/// [`WorkActs::unreadable`] instead (ARCH §18.3).
pub fn from_payload(payload: &Payload) -> Option<WorkAct> {
    kind_of(payload)?;
    serde_json::from_value(payload.as_value().clone()).ok()
}

/// What kind of act this line is, without decoding it.
///
/// One string lookup, no `serde_json::from_value`, no allocation of the body —
/// which is what makes it usable from a log renderer walking a whole journal.
pub fn kind_of(payload: &Payload) -> Option<WorkActKind> {
    WorkActKind::parse(payload.as_value().as_object()?.get("kind")?.as_str()?)
}

/// One act a peer put on this journal, with everything admission already
/// established about it.
///
/// `actor` and `person` come from ADMISSION, never from the payload: the
/// signature is the only field on a line a writer cannot forge for somebody
/// else, and reading a claimed author out of the body would hand consent to
/// the writer (ARCH §18.1).
#[derive(Debug, Clone)]
pub struct RailWorkAct {
    /// Content-derived op id — what a `Correct` names.
    pub id: OpId,
    pub actor: ActorKey,
    /// Who the roster says that key is.
    pub person: Person,
    pub seq: u64,
    /// The op's timestamp, in the rail's units. Carried, never read from a
    /// clock: expiry is derived by every node from this and the fold's window.
    pub ts_unix: i64,
    pub act: WorkAct,
}

/// What one read of the `work` journal found.
#[derive(Debug, Clone, Default)]
pub struct WorkActs {
    /// The acts, **in admission order**. Not sorted here — see the module
    /// docs.
    pub found: Vec<RailWorkAct>,
    /// Admitted lines this build could not read as a work act: a peer on a
    /// newer vocabulary, or a line whose signer this build cannot spell.
    pub unreadable: usize,
    /// Everything admission could not account for — an unplaceable signer, a
    /// hole in a peer's sequence, a torn line. Reported rather than swallowed:
    /// an empty `found` beside a non-zero `gaps` is a very different fact from
    /// an empty `found` beside a quiet ring (ARCH §18.3).
    pub gaps: usize,
}

/// Read an admission as work acts.
///
/// Voided ops are already excluded: `Admission::applied()` is the one
/// definition of "surviving", so a correction voids an act here exactly as it
/// does everywhere else on the rail, and this crate holds no second void rule.
/// `RailAct::Seal` carries no payload and so is never seen — it is delivery,
/// not meaning.
pub fn read(admission: &Admission) -> WorkActs {
    let mut out = WorkActs {
        gaps: admission.gaps.len(),
        ..Default::default()
    };
    for op in admission.applied() {
        let Some(payload) = op.payload.as_ref() else {
            continue;
        };
        let Some(act) = from_payload(payload) else {
            // Either a line from a peer whose vocabulary this build does not
            // have, or a payload on this namespace that is not a work act at
            // all. Both are facts to count, not to fail on.
            tracing::debug!(
                target: crate::TRACE_TARGET,
                actor = %op.actor,
                seq = op.seq,
                "work act unreadable"
            );
            out.unreadable += 1;
            continue;
        };
        let Ok(actor) = ActorKey::of_op(op) else {
            tracing::debug!(
                target: crate::TRACE_TARGET,
                actor = %op.actor,
                seq = op.seq,
                "work act signer key unspellable"
            );
            out.unreadable += 1;
            continue;
        };
        out.found.push(RailWorkAct {
            id: op.id.clone(),
            actor,
            person: op.person.clone(),
            seq: op.seq,
            ts_unix: op.ts_unix,
            act,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::judgement::Reason;
    use kernel_types::{NodeId, Server};
    use oicp_types::JobRequirements;
    use serde_json::json;

    fn kind(raw: &str) -> JobKind {
        JobKind::parse(raw).expect("test kind")
    }

    fn unit(body: Value) -> JobUnit {
        seal::seal(kind("process:v1"), body, JobRequirements::any(), None).expect("sealed")
    }

    fn handoff() -> HandoffId {
        HandoffId::from_u128(7)
    }

    fn provenance() -> ComputeAttribution {
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

    fn submission() -> Submission {
        Submission::new(
            handoff(),
            kind("process:v1"),
            vec![unit(json!({ "argv": ["uname", "-a"] }))],
            None,
            None,
        )
    }

    /// Every act must round-trip through the rail's own canonical form, or the
    /// plane loses acts on a boundary nobody is watching.
    #[test]
    fn every_act_round_trips_through_a_payload() {
        let acts = vec![
            WorkAct::Submit(submission()),
            WorkAct::Offer(WorkOffer {
                kinds: vec![kind("process:v1")],
                max_concurrent: 2,
                yield_to_foreground: true,
                isolation: oicp_types::Isolation::Subprocess,
                os: "linux".into(),
                arch: "x86_64".into(),
                repos: vec![],
                accept_from: None,
            }),
            WorkAct::Lease(UnitRef {
                handoff: handoff(),
                unit_hash: "ab".repeat(32),
            }),
            WorkAct::Renew(UnitRef {
                handoff: handoff(),
                unit_hash: "ab".repeat(32),
            }),
            WorkAct::Complete(Completion {
                handoff: handoff(),
                unit_hash: "ab".repeat(32),
                outcome: Judgement::passed("unit", Reason::literal("8412 passed, 0 failed")),
                result: json!({ "exit_code": 0 }),
                provenance: provenance(),
            }),
            WorkAct::Fail(Failure {
                handoff: handoff(),
                unit_hash: "ab".repeat(32),
                outcome: Judgement::could_not_judge("unit", Reason::literal("killed on timeout")),
                provenance: provenance(),
            }),
            WorkAct::Revoke(Revocation { handoff: handoff() }),
        ];
        assert_eq!(acts.len(), WorkActKind::ALL.len(), "one act per kind");

        for act in &acts {
            let payload = to_payload(act).unwrap_or_else(|e| panic!("{:?}: {e}", act.kind()));
            assert_eq!(
                kind_of(&payload),
                Some(act.kind()),
                "the discriminator must be readable without decoding"
            );
            let back = from_payload(&payload).expect("decodes");
            assert_eq!(back.kind(), act.kind());
            assert_eq!(
                serde_json::to_value(&back).unwrap(),
                serde_json::to_value(act).unwrap()
            );
        }
    }

    /// The discriminator is a top-level string key, so a log renderer can read
    /// it off the raw JSON without this crate's types at all.
    #[test]
    fn the_kind_is_a_top_level_string_on_the_payload() {
        let payload = to_payload(&WorkAct::Revoke(Revocation { handoff: handoff() })).unwrap();
        assert_eq!(
            payload.as_value().get("kind").and_then(Value::as_str),
            Some("work.revoke")
        );
    }

    /// A line from a namespace-mate this build does not know is COUNTED, not
    /// fatal. Failing input: a well-formed rail payload whose kind is not ours.
    #[test]
    fn an_unknown_act_kind_is_unreadable_rather_than_an_error() {
        let alien = Payload::new(json!({ "kind": "work.teleport", "handoff": "x" })).unwrap();
        assert_eq!(kind_of(&alien), None);
        assert!(from_payload(&alien).is_none());
    }

    /// Failing input: a sealed unit whose payload is edited afterwards. It must
    /// not reach a journal every peer keeps forever.
    #[test]
    fn a_submission_carrying_an_unsealed_unit_is_refused_at_the_door() {
        let mut s = submission();
        s.units[0].payload = json!({ "argv": ["rm", "-rf", "/"] });
        let err = to_payload(&WorkAct::Submit(s)).unwrap_err();
        assert!(err.contains("seals as"), "{err}");
    }

    /// Failing input: an `ingest:v1` unit smuggled into a `process:v1`
    /// handoff. A donor offering only `process:v1` would otherwise lease it.
    #[test]
    fn a_submission_whose_unit_disagrees_with_its_kind_is_refused() {
        let s = Submission::new(
            handoff(),
            kind("process:v1"),
            vec![seal::seal(
                kind("ingest:v1"),
                json!({ "argv": ["true"] }),
                JobRequirements::any(),
                None,
            )
            .unwrap()],
            None,
            None,
        );
        let err = to_payload(&WorkAct::Submit(s)).unwrap_err();
        assert!(
            err.contains("ingest:v1") && err.contains("process:v1"),
            "{err}"
        );
    }

    /// Failing input: `timeout_secs: 1.5` inside a unit body, arriving at the
    /// rail door rather than at the seal. Same refusal, same sentence — there
    /// is one canonicalizer and both paths reach it.
    #[test]
    fn a_fractional_number_is_refused_at_the_rail_door_too() {
        // The unit cannot even be sealed, which is the first line of defence.
        assert!(seal::seal(
            kind("process:v1"),
            json!({ "argv": ["sleep"], "timeout_secs": 1.5 }),
            JobRequirements::any(),
            None
        )
        .is_err());

        // And a hand-built one does not get past `to_payload` either.
        let mut s = submission();
        s.units[0].payload = json!({ "argv": ["sleep"], "timeout_secs": 1.5 });
        let err = to_payload(&WorkAct::Submit(s)).unwrap_err();
        assert!(err.contains("1.5"), "{err}");
    }

    #[test]
    fn an_empty_submission_is_refused() {
        let s = Submission::new(handoff(), kind("process:v1"), vec![], None, None);
        assert!(to_payload(&WorkAct::Submit(s)).is_err());
    }

    /// The tri-state `allowed` field: all three states mean something, and the
    /// empty list is NOT the same as absent.
    #[test]
    fn the_allow_list_keeps_its_three_states() {
        let me = ActorKey::parse("ab".repeat(32)).unwrap();
        let other = ActorKey::parse("cd".repeat(32)).unwrap();

        let mut open = submission();
        open.allowed = None;
        assert!(open.allows(&me) && open.allows(&other));

        let mut named = submission();
        named.allowed = Some(vec![me.clone()]);
        assert!(named.allows(&me) && !named.allows(&other));

        let mut self_only = submission();
        self_only.allowed = Some(vec![]);
        assert!(!self_only.allows(&me) && !self_only.allows(&other));
    }

    #[test]
    fn a_ttl_is_clamped_the_way_the_work_atlas_clamps_one() {
        let s = |ttl| Submission::new(handoff(), kind("process:v1"), vec![], None, ttl).ttl_secs;
        assert_eq!(s(None), DEFAULT_TTL_SECS);
        assert_eq!(s(Some(60)), 60);
        assert_eq!(s(Some(0)), MIN_TTL_SECS);
        assert_eq!(s(Some(999_999)), MAX_TTL_SECS);
    }

    #[test]
    fn every_act_kind_parses_back_from_its_wire_spelling() {
        for k in WorkActKind::ALL {
            assert_eq!(WorkActKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(
            WorkActKind::parse("submit"),
            None,
            "the prefix is load-bearing"
        );
    }
}
