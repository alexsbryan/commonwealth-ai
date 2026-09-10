// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`WorkRefusal`] and [`may_take`] — why a donor may not take a unit.
//!
//! One closed refusal vocabulary (ARCH §2.1) and one predicate over the rail's
//! own state. A donor calls [`may_take`] before it appends a `Lease`, and
//! `svrn job status` calls it to explain a unit that is sitting still — the
//! same rule answering both questions, which is the validate/writable split
//! the expenses ring template ships (`ring_cmd/templates/expenses.js:84,157`):
//! one predicate, one caller that acts on it and one that renders it, never
//! two implementations that drift.
//!
//! # What `may_take` can and cannot judge, and why that is not a second rule
//!
//! Its parameters are the fold, the donor's own key, the donor's own offer and
//! `now_ms`. That is the whole of the rail's state, and eight of the ten
//! refusals below are decided from it.
//!
//! Three are **not facts about the rail**, and no signature over the rail
//! could decide them:
//!
//! - [`WorkRefusal::IsolationBelow`] compares a unit's required isolation
//!   against the donor's. A unit does not carry one — `JobExecutorDescriptor`
//!   does — so it is the executor registry that decides it, at registration.
//! - [`UnmetRequirement::Precondition`] and [`UnmetRequirement::RepoRev`] ask
//!   whether THIS host has a binary, a container, a checkout at a rev. Only
//!   the host knows.
//! - [`WorkRefusal::Yielding`] asks whether the operator is at the keyboard
//!   right now. Only the daemon knows (`AppState::should_yield_to_foreground`).
//!
//! Those four are constructed by the caller that holds the knowledge —
//! `JobExecutor::validate` and the donor loop — **in this same closed type**,
//! so there is still one refusal vocabulary, one `id()` table and one set of
//! sentences reaching an operator. A second enum for "host refusals" is the
//! thing this module exists to prevent (ARCH §10.6).

use kernel_types::quality::Precondition;
use kernel_types::Judgement;
use oicp_types::{Isolation, JobKind};

use crate::act::UnitRef;
use crate::actor::ActorKey;
use crate::projection::{WorkProjection, WorkUnitStatus};
use crate::seal;

// -----------------------------------------------------------------
// The vocabulary
// -----------------------------------------------------------------

/// Which half of the grant said no.
///
/// The grant on this plane is `Submit.allowed` ∩ `Offer.accept_from` and
/// there is no `Grant` noun, because the intersection of two existing fields
/// is not a third thing. It does mean a bare "not allowed" is ambiguous about
/// which side refused, and an operator debugging a donor that takes nothing
/// needs exactly that — so the side is a field rather than a sentence a reader
/// has to reconstruct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantSide {
    /// The submitter's half: their `allowed` list does not name this donor,
    /// the handoff was revoked, its TTL elapsed, or no admitted `Submit`
    /// names this unit at all. All four are one fact — the submitter has not
    /// consented to this donor taking this unit now.
    Submitter,
    /// The donor's half: its own `Offer.accept_from` does not name the
    /// submitter.
    Donor,
}

impl GrantSide {
    pub fn id(&self) -> &'static str {
        match self {
            GrantSide::Submitter => "submitter",
            GrantSide::Donor => "donor",
        }
    }
}

/// Which requirement the host did not satisfy.
///
/// [`JobRequirements`](oicp_types::JobRequirements) has four halves and the
/// order's spelling of the refusal — `RequirementUnmet(Precondition)` — can
/// only carry one of them. This is that spelling with the other three beside
/// it, so `repo_rev`, `os` and `arch` refusals are as typed and as renderable
/// as a missing binary; the variant count of [`WorkRefusal`] stays at ten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnmetRequirement {
    /// The unit is pinned to an OS this host is not running.
    Os { required: String, host: String },
    /// The unit is pinned to an architecture this host is not.
    Arch { required: String, host: String },
    /// The unit is pinned to a revision this host's checkout is not at. A
    /// result computed at another rev is not evidence about the submitter's
    /// tree — the fact `ComputeAttribution::comparable_to` exists to state.
    RepoRev { required: String, host: String },
    /// A `quality/instruments.toml` precondition — a binary, a container, a
    /// listening port — that this host does not meet. The registry's own
    /// vocabulary, not a second one.
    Precondition(Precondition),
}

impl UnmetRequirement {
    pub fn id(&self) -> &'static str {
        match self {
            UnmetRequirement::Os { .. } => "os",
            UnmetRequirement::Arch { .. } => "arch",
            UnmetRequirement::RepoRev { .. } => "repo-rev",
            UnmetRequirement::Precondition(_) => "precondition",
        }
    }
}

impl std::fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnmetRequirement::Os { required, host } => {
                write!(
                    f,
                    "the unit needs os `{required}` and this host is `{host}`"
                )
            }
            UnmetRequirement::Arch { required, host } => {
                write!(
                    f,
                    "the unit needs arch `{required}` and this host is `{host}`"
                )
            }
            UnmetRequirement::RepoRev { required, host } => write!(
                f,
                "the unit is pinned to rev `{required}` and this checkout is at `{host}`"
            ),
            UnmetRequirement::Precondition(p) => {
                write!(f, "this host does not satisfy `{}`", p.label())
            }
        }
    }
}

/// Why a donor may not take a unit. Closed (ARCH §2.1), ten variants, each
/// with a stable [`id`](WorkRefusal::id) in `ResourceVerdict::id()`'s pattern
/// (`sovereign-work-atlas/src/tools/resource_may_i.rs:78`).
///
/// Every one renders as a sentence an operator can act on, because these
/// reach a person: through `svrn job status`, through the donor's debug log,
/// and through the refusal a submitter reads when nothing is being taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkRefusal {
    /// This donor's offer does not name this kind at all.
    #[error("this donor does not offer `{kind}`")]
    KindNotOffered { kind: JobKind },
    /// The donor offers the same kind at a different revision. A DIFFERENT
    /// refusal from [`KindNotOffered`](WorkRefusal::KindNotOffered) on
    /// purpose: one means "wrong machine", the other means "update one of
    /// us", and `JobKind::is_skew_of` is the one place the difference is
    /// decided.
    #[error("this donor offers `{offered}` and the unit is `{wanted}` — same kind, different contract revision")]
    VersionSkew { wanted: JobKind, offered: JobKind },
    /// Nobody consented. See [`GrantSide`] for which half.
    #[error("`{actor}` is not inside the {} half of the grant", side.id())]
    NotAllowed { actor: ActorKey, side: GrantSide },
    /// The donor cannot isolate the unit as strongly as its executor requires.
    /// Constructed by the executor registry — see the module docs.
    #[error("this unit needs `{required:?}` isolation and this donor offers `{offered:?}`")]
    IsolationBelow {
        required: Isolation,
        offered: Isolation,
    },
    /// The host does not satisfy one of the unit's requirements.
    #[error("{0}")]
    RequirementUnmet(UnmetRequirement),
    /// Somebody else's lease is still live. Not an error — the normal outcome
    /// of two donors reading one queue.
    #[error("`{holder}` holds a live lease on this unit until {expires_at_ms}ms")]
    AlreadyLeased {
        holder: ActorKey,
        expires_at_ms: u64,
    },
    /// The unit is terminal and will not be offered again.
    ///
    /// `outcome` is what keeps the work atlas's `Expired`-vs-`Free`
    /// distinction (`resource_may_i.rs:65`) visible here: `Some` means
    /// somebody reported a verdict and the unit is finished, `None` means the
    /// lease lapsed `MAX_UNIT_ATTEMPTS` times and **nobody ever reported** —
    /// which is a fact about the cohort, not about the unit.
    #[error("{}", abandoned_sentence(*attempts, outcome))]
    Abandoned {
        attempts: u32,
        outcome: Option<Judgement>,
    },
    /// The unit's payload is not something that can be run — its declared
    /// identity does not cover its body, or the executor's schema refuses it.
    /// `detail` carries the sentence that names the fix.
    #[error("this unit's payload cannot be run: {detail}")]
    PayloadNotCanonical { detail: String },
    /// The donor is already holding as many leases as its offer allows.
    #[error("this donor holds {held} of the {max} concurrent leases it offered")]
    Concurrency { held: u32, max: u32 },
    /// The donor's operator is at the keyboard and its offer says to stand
    /// down. Constructed by the donor loop — see the module docs.
    #[error("this donor is yielding to foreground work")]
    Yielding,
}

/// The sentence for [`WorkRefusal::Abandoned`], which branches on whether
/// anybody ever reported. Free function because `thiserror`'s `#[error]` takes
/// a format expression and not a match.
fn abandoned_sentence(attempts: u32, outcome: &Option<Judgement>) -> String {
    match outcome {
        Some(j) => format!(
            "this unit already reached a verdict after {attempts} attempt(s): {} — {}",
            j.verdict().as_str(),
            j.reason().as_str()
        ),
        None => format!(
            "this unit's lease lapsed {attempts} time(s) and nobody ever reported, so it is \
             terminal — the donors that took it went silent rather than finishing"
        ),
    }
}

impl WorkRefusal {
    /// A stable, greppable id — `ResourceVerdict::id()`'s pattern. It is what
    /// a table column, a metric label and a `--json` field carry, so it never
    /// changes when a sentence is reworded.
    pub fn id(&self) -> &'static str {
        match self {
            WorkRefusal::KindNotOffered { .. } => "kind-not-offered",
            WorkRefusal::VersionSkew { .. } => "version-skew",
            WorkRefusal::NotAllowed { .. } => "not-allowed",
            WorkRefusal::IsolationBelow { .. } => "isolation-below",
            WorkRefusal::RequirementUnmet(_) => "requirement-unmet",
            WorkRefusal::AlreadyLeased { .. } => "already-leased",
            WorkRefusal::Abandoned { .. } => "abandoned",
            WorkRefusal::PayloadNotCanonical { .. } => "payload-not-canonical",
            WorkRefusal::Concurrency { .. } => "concurrency",
            WorkRefusal::Yielding => "yielding",
        }
    }

    /// Every id, so a renderer can enumerate the vocabulary without a match
    /// and a test can prove the ids are distinct.
    pub const ALL_IDS: [&'static str; 10] = [
        "kind-not-offered",
        "version-skew",
        "not-allowed",
        "isolation-below",
        "requirement-unmet",
        "already-leased",
        "abandoned",
        "payload-not-canonical",
        "concurrency",
        "yielding",
    ];
}

// -----------------------------------------------------------------
// The predicate
// -----------------------------------------------------------------

/// **The** question "may I take this unit?", answered from the rail.
///
/// Run donor-side before appending a `Lease`, and by `svrn job status` to say
/// why a unit is sitting still. `Ok(())` means every rail-side condition is
/// met — the questions a signature over the rail CAN answer.
///
/// The host-side half is [`host_satisfies`], in this file behind the `process`
/// feature. It used to be "the caller's to check with this same type", which
/// shared the vocabulary and left the DECIDER to each donor; cw-lift 5f showed
/// what that costs, because the second donor to exist re-derived it and
/// checked one precondition of the three this build can evaluate. Sharing the
/// type was never the hard part.
///
/// Nothing here reads a clock: `now_ms` is a parameter, so two nodes asking
/// about the same journal at the same instant get the same answer.
pub fn may_take(
    proj: &WorkProjection,
    self_key: &ActorKey,
    self_offer: &oicp_types::WorkOffer,
    unit: &UnitRef,
    now_ms: u64,
) -> Result<(), WorkRefusal> {
    let verdict = decide(proj, self_key, self_offer, unit, now_ms);
    if let Err(refusal) = &verdict {
        tracing::debug!(
            target: crate::TRACE_TARGET,
            handoff = %unit.handoff,
            unit = %unit.unit_hash,
            actor = %self_key,
            refusal = refusal.id(),
            why = %refusal,
            "work unit refused"
        );
    }
    verdict
}

fn decide(
    proj: &WorkProjection,
    self_key: &ActorKey,
    self_offer: &oicp_types::WorkOffer,
    unit: &UnitRef,
    now_ms: u64,
) -> Result<(), WorkRefusal> {
    // A unit no admitted `Submit` names is a unit whose submitter has not
    // consented to anyone taking it — the same absence of a grant as an
    // `allowed` list that omits you, and deliberately not an eleventh variant.
    let not_submitted = || WorkRefusal::NotAllowed {
        actor: self_key.clone(),
        side: GrantSide::Submitter,
    };
    let handoff = proj.handoffs.get(&unit.handoff).ok_or_else(not_submitted)?;
    let projected = handoff
        .units
        .get(&unit.unit_hash)
        .ok_or_else(not_submitted)?;

    // 1. Identity, before anything that depends on it. A unit whose hash does
    //    not cover its payload cannot be leased or completed idempotently, so
    //    every later check would be about a body nobody agreed to.
    seal::verify(&projected.unit).map_err(|e| WorkRefusal::PayloadNotCanonical {
        detail: e.to_string(),
    })?;

    // 2. What this donor runs at all. `is_skew_of` is the one place the
    //    difference between "wrong machine" and "update one of us" is decided.
    let wanted = &projected.unit.kind;
    if !self_offer.offers_kind(wanted) {
        return Err(
            match self_offer.kinds.iter().find(|k| wanted.is_skew_of(k)) {
                Some(offered) => WorkRefusal::VersionSkew {
                    wanted: wanted.clone(),
                    offered: offered.clone(),
                },
                None => WorkRefusal::KindNotOffered {
                    kind: wanted.clone(),
                },
            },
        );
    }

    // 3. Both halves of the grant. `Submit.allowed` ∩ `Offer.accept_from`,
    //    read through the accessors that own each tri-state.
    if !handoff.admits(self_key, now_ms) {
        return Err(not_submitted());
    }
    if !self_offer.accepts_from(handoff.submitter.as_str()) {
        return Err(WorkRefusal::NotAllowed {
            actor: handoff.submitter.clone(),
            side: GrantSide::Donor,
        });
    }

    // 4. The platform half of the requirements. `accepts_host` is the one
    //    place "absent means any" is written, so it is the decider; the
    //    per-field comparison below runs only to name WHICH half failed, and
    //    only once the decider has already said no.
    let requirements = &projected.unit.requirements;
    if !requirements.accepts_host(&self_offer.os, &self_offer.arch) {
        let unmet = match requirements.os.as_deref() {
            Some(want) if want != self_offer.os => UnmetRequirement::Os {
                required: want.to_string(),
                host: self_offer.os.clone(),
            },
            _ => UnmetRequirement::Arch {
                required: requirements.arch.clone().unwrap_or_default(),
                host: self_offer.arch.clone(),
            },
        };
        return Err(WorkRefusal::RequirementUnmet(unmet));
    }

    // 5. The queue. Expiry is derived here exactly as every other reader
    //    derives it — `status_at` is the one place.
    match projected.status_at(now_ms) {
        WorkUnitStatus::Queued { .. } => {}
        WorkUnitStatus::Leased {
            lessee,
            expires_at_ms,
            ..
        } => {
            return Err(WorkRefusal::AlreadyLeased {
                holder: lessee,
                expires_at_ms,
            })
        }
        WorkUnitStatus::Complete {
            outcome, attempts, ..
        } => {
            return Err(WorkRefusal::Abandoned {
                attempts,
                outcome: Some(outcome),
            })
        }
        WorkUnitStatus::Failed {
            attempts, outcome, ..
        } => return Err(WorkRefusal::Abandoned { attempts, outcome }),
    }

    // 6. The donor's own budget, last: it is the only check whose answer
    //    changes as this donor works, so a caller retrying a refusal wants to
    //    have exhausted the permanent ones first.
    let held = proj.leases_held_by(self_key, now_ms);
    if held >= self_offer.max_concurrent {
        return Err(WorkRefusal::Concurrency {
            held,
            max: self_offer.max_concurrent,
        });
    }

    Ok(())
}

// -----------------------------------------------------------------
// The host's half of the same predicate
// -----------------------------------------------------------------

/// The requirements only this HOST can answer — the other half of
/// [`may_take`].
///
/// `may_take` decides everything a signature over the rail can see: kind,
/// both sides of the grant, os, arch, the queue, the lease budget. What is
/// left is the precondition list, and no act carries the answer — whether
/// `python3` is on this machine's `PATH` is not a fact the submitter can
/// sign.
///
/// **A precondition this build cannot EVALUATE is refused, never assumed
/// met** (ARCH §18.3). A donor that read "I cannot check this" as "it is
/// fine" would lease the unit, run it, and return a verdict about a machine
/// that did not meet the unit's terms — which is worse than not taking it,
/// because the submitter gets a green.
///
/// # Why it lives here and not in each donor
///
/// It was sovereign-side until cw-lift 5f, and the vocabulary
/// ([`UnmetRequirement::Precondition`]) was shared while the decider was not.
/// The second donor to exist — the lifted peer — then re-derived it and
/// checked exactly one of the three kinds this build can evaluate, silently
/// refusing every containerised unit it could in fact have run. Behind the
/// `process` feature with the executor, because it reads `PATH` and a file,
/// so a lifter of the fold alone still links no I/O.
#[cfg(feature = "process")]
pub fn host_satisfies(unit: &oicp_types::JobUnit) -> Result<(), WorkRefusal> {
    for precondition in &unit.requirements.preconditions {
        let met = match precondition {
            Precondition::Binary(name) => binary_on_path(name),
            Precondition::Container(name) => in_container(name),
            // Not evaluable from a package crate: a listening port is a
            // socket probe, and slots and corpora are an agent runtime's
            // state, which this closure deliberately cannot reach. Refused
            // and NAMED rather than assumed — the caller sees which
            // precondition stopped it, not a bare false.
            Precondition::PortListening(_)
            | Precondition::SlotDecodes(_)
            | Precondition::CorpusInstalled(_) => false,
        };
        if !met {
            return Err(WorkRefusal::RequirementUnmet(
                UnmetRequirement::Precondition(precondition.clone()),
            ));
        }
    }
    Ok(())
}

/// Is `name` an executable on this host's `PATH`?
#[cfg(feature = "process")]
fn binary_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Is this process inside the named toolbox/container?
///
/// `/run/.containerenv` names the container and its absence means the host —
/// the same read `AGENTS.md` documents for a person checking by hand, so
/// there is one answer to "which side am I on" (ARCH §10.6).
#[cfg(feature = "process")]
fn in_container(name: &str) -> bool {
    std::fs::read_to_string("/run/.containerenv")
        .map(|text| text.contains(&format!("name=\"{name}\"")))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::act::{Revocation, Submission, WorkAct};
    use crate::projection::tests_fixture::*;
    use commonwealth_core::knowledge::LEASE_MS;
    use commonwealth_rail_core::{Op, SignedOp};
    use oicp_types::{JobRequirements, JobUnit, WorkOffer};
    use serde_json::json;

    fn offer_of(
        kinds: &[&str],
        accept_from: Option<Vec<String>>,
        max_concurrent: u32,
    ) -> WorkOffer {
        WorkOffer {
            max_concurrent,
            ..offer(kinds, accept_from)
        }
    }

    /// A one-unit handoff of `k`, submitted by alex at t=100s with `allowed`.
    /// Returns the journal so far, so a test can append leases to it.
    fn submitted(
        k: &str,
        requirements: JobRequirements,
        allowed: Option<Vec<ActorKey>>,
    ) -> (Vec<Op<SignedOp>>, JobUnit) {
        let unit =
            seal::seal(kind(k), json!({ "argv": ["true"] }), requirements, None).expect("sealed");
        let act = WorkAct::Submit(Submission::new(
            handoff(),
            kind(k),
            vec![unit.clone()],
            allowed,
            None,
        ));
        (vec![op(1, 100, 0, &act)], unit)
    }

    /// The journal so far plus `extra`, folded.
    fn with(ops: &[Op<SignedOp>], extra: Vec<Op<SignedOp>>) -> WorkProjection {
        let mut all = ops.to_vec();
        all.extend(extra);
        fold(&all)
    }

    /// The ten ids are distinct and complete — a renderer keying on them
    /// cannot silently collapse two refusals into one column, and the list
    /// cannot drift from the enum.
    #[test]
    fn every_refusal_has_its_own_stable_id() {
        let mut seen = WorkRefusal::ALL_IDS.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), WorkRefusal::ALL_IDS.len(), "ids are distinct");

        let every = [
            WorkRefusal::KindNotOffered {
                kind: kind("process:v1"),
            },
            WorkRefusal::VersionSkew {
                wanted: kind("process:v2"),
                offered: kind("process:v1"),
            },
            WorkRefusal::NotAllowed {
                actor: who(2),
                side: GrantSide::Donor,
            },
            WorkRefusal::IsolationBelow {
                required: Isolation::Vm,
                offered: Isolation::Subprocess,
            },
            WorkRefusal::RequirementUnmet(UnmetRequirement::Precondition(Precondition::Container(
                "sovereign-vulkan".into(),
            ))),
            WorkRefusal::AlreadyLeased {
                holder: who(2),
                expires_at_ms: 1,
            },
            WorkRefusal::Abandoned {
                attempts: 3,
                outcome: None,
            },
            WorkRefusal::PayloadNotCanonical {
                detail: "the payload declares one identity and seals as another".into(),
            },
            WorkRefusal::Concurrency { held: 2, max: 2 },
            WorkRefusal::Yielding,
        ];
        assert_eq!(every.len(), WorkRefusal::ALL_IDS.len(), "one of each");
        for refusal in &every {
            assert!(
                WorkRefusal::ALL_IDS.contains(&refusal.id()),
                "{} is not in ALL_IDS",
                refusal.id()
            );
            assert!(
                !refusal.to_string().is_empty(),
                "every refusal renders as a sentence"
            );
        }
        // The four `UnmetRequirement` halves are distinct too, so a table
        // reading `requirement-unmet` can still say which half refused.
        let halves = [
            UnmetRequirement::Os {
                required: "macos".into(),
                host: "linux".into(),
            },
            UnmetRequirement::Arch {
                required: "aarch64".into(),
                host: "x86_64".into(),
            },
            UnmetRequirement::RepoRev {
                required: "abc".into(),
                host: "def".into(),
            },
            UnmetRequirement::Precondition(Precondition::Binary("python3".into())),
        ];
        let mut ids: Vec<&str> = halves.iter().map(UnmetRequirement::id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), halves.len());
    }

    /// **Named test.** Failing input: an `ingest:v1` unit offered to a donor
    /// that runs only `process:v1`. It must refuse BEFORE any lease is
    /// appended — a donor that leases first and discovers the kind afterwards
    /// has already taken a unit off the queue for `LEASE_MS`.
    #[test]
    fn a_kind_this_donor_does_not_offer_is_refused_without_leasing() {
        let (ops, unit) = submitted("ingest:v1", JobRequirements::any(), None);
        let proj = fold(&ops);
        let err = may_take(
            &proj,
            &who(2),
            &offer_of(&["process:v1"], None, 4),
            &unit_ref(&unit),
            100_000,
        )
        .unwrap_err();

        assert_eq!(
            err,
            WorkRefusal::KindNotOffered {
                kind: kind("ingest:v1")
            }
        );
        assert!(err.to_string().contains("ingest:v1"), "{err}");
        // Nothing was taken: the queue is exactly as it was.
        assert_eq!(proj.takeable_at(100_000), vec![unit_ref(&unit)]);
        assert!(proj.lost_leases.is_empty());
    }

    /// **Named test.** Failing input: an `ingest:v2` unit offered to a donor
    /// that runs `ingest:v1`. Same id, different contract revision — a
    /// DIFFERENT refusal from "wrong machine", because the fix is different.
    #[test]
    fn the_same_kind_at_another_revision_is_a_version_skew() {
        let (ops, unit) = submitted("ingest:v2", JobRequirements::any(), None);
        let err = may_take(
            &fold(&ops),
            &who(2),
            &offer_of(&["ingest:v1"], None, 4),
            &unit_ref(&unit),
            100_000,
        )
        .unwrap_err();

        assert_eq!(
            err,
            WorkRefusal::VersionSkew {
                wanted: kind("ingest:v2"),
                offered: kind("ingest:v1"),
            }
        );
        assert_ne!(
            err.id(),
            WorkRefusal::KindNotOffered {
                kind: kind("ingest:v2")
            }
            .id(),
            "a skew and an unoffered kind must not render as one refusal"
        );
    }

    /// **Named test.** Failing input: a donor whose `accept_from` lists
    /// somebody other than the submitter. The donor's half of the grant is
    /// its own consent and the submitter does not get a vote on it.
    #[test]
    fn an_accept_list_excluding_the_submitter_refuses_the_donor_side() {
        let (ops, unit) = submitted("process:v1", JobRequirements::any(), None);
        let proj = fold(&ops);
        // alex (key 1) submitted; this donor accepts only cy.
        let closed = offer_of(&["process:v1"], Some(vec![who(3).to_string()]), 4);
        assert_eq!(
            may_take(&proj, &who(2), &closed, &unit_ref(&unit), 100_000),
            Err(WorkRefusal::NotAllowed {
                actor: who(1),
                side: GrantSide::Donor,
            })
        );

        // The same donor with the submitter on the list takes it.
        let open = offer_of(&["process:v1"], Some(vec![who(1).to_string()]), 4);
        assert_eq!(
            may_take(&proj, &who(2), &open, &unit_ref(&unit), 100_000),
            Ok(())
        );
    }

    /// The submitter's half, and its three shapes. Failing inputs: an
    /// `allowed` list that names somebody else, and `Some(∅)` — self-only,
    /// which is NOT the same as open.
    #[test]
    fn a_submission_that_does_not_name_this_donor_refuses_the_submitter_side() {
        let any = offer_of(&["process:v1"], None, 4);

        let (ops, unit) = submitted("process:v1", JobRequirements::any(), Some(vec![who(3)]));
        let named = fold(&ops);
        assert_eq!(
            may_take(&named, &who(2), &any, &unit_ref(&unit), 100_000),
            Err(WorkRefusal::NotAllowed {
                actor: who(2),
                side: GrantSide::Submitter,
            })
        );
        assert_eq!(
            may_take(&named, &who(3), &any, &unit_ref(&unit), 100_000),
            Ok(())
        );

        let (ops, unit) = submitted("process:v1", JobRequirements::any(), Some(vec![]));
        let self_only = fold(&ops);
        assert!(may_take(&self_only, &who(2), &any, &unit_ref(&unit), 100_000).is_err());
        assert!(
            may_take(&self_only, &who(1), &any, &unit_ref(&unit), 100_000).is_err(),
            "`Some(vec![])` is self-only, and self is the caller's own knowledge \
             of who it is — not a membership the list encodes"
        );
    }

    /// A unit nobody submitted is refused as a missing grant, not as a panic
    /// and not as an eleventh variant. Failing input: a `UnitRef` naming a
    /// hash this journal does not carry.
    #[test]
    fn a_unit_no_submission_names_is_a_missing_grant() {
        let (ops, _unit) = submitted("process:v1", JobRequirements::any(), None);
        let ghost = UnitRef {
            handoff: handoff(),
            unit_hash: "ab".repeat(32),
        };
        assert_eq!(
            may_take(
                &fold(&ops),
                &who(2),
                &offer_of(&["process:v1"], None, 4),
                &ghost,
                100_000
            ),
            Err(WorkRefusal::NotAllowed {
                actor: who(2),
                side: GrantSide::Submitter,
            })
        );
    }

    /// The platform half of the requirements, naming WHICH half refused.
    /// Failing input: a unit pinned to `macos` offered to a linux donor.
    #[test]
    fn a_unit_pinned_to_another_platform_names_the_half_that_refused() {
        let (ops, unit) = submitted(
            "process:v1",
            JobRequirements {
                os: Some("macos".into()),
                ..JobRequirements::any()
            },
            None,
        );
        let err = may_take(
            &fold(&ops),
            &who(2),
            &offer_of(&["process:v1"], None, 4),
            &unit_ref(&unit),
            100_000,
        )
        .unwrap_err();
        assert_eq!(
            err,
            WorkRefusal::RequirementUnmet(UnmetRequirement::Os {
                required: "macos".into(),
                host: "linux".into(),
            })
        );
        assert!(err.to_string().contains("macos"), "{err}");

        let (ops, unit) = submitted(
            "process:v1",
            JobRequirements {
                arch: Some("aarch64".into()),
                ..JobRequirements::any()
            },
            None,
        );
        assert_eq!(
            may_take(
                &fold(&ops),
                &who(2),
                &offer_of(&["process:v1"], None, 4),
                &unit_ref(&unit),
                100_000
            ),
            Err(WorkRefusal::RequirementUnmet(UnmetRequirement::Arch {
                required: "aarch64".into(),
                host: "x86_64".into(),
            }))
        );
    }

    /// A live lease refuses; the same lease past its deadline does not, and
    /// the attempt is carried. Failing input: reading at the deadline
    /// millisecond, which belongs to nobody.
    #[test]
    fn a_live_lease_refuses_and_a_lapsed_one_does_not() {
        let (ops, unit) = submitted("process:v1", JobRequirements::any(), None);
        let leased = with(&ops, vec![op(3, 200, 0, &WorkAct::Lease(unit_ref(&unit)))]);
        let any = offer_of(&["process:v1"], None, 4);
        let deadline = 200_000 + LEASE_MS;

        assert_eq!(
            may_take(&leased, &who(2), &any, &unit_ref(&unit), deadline - 1),
            Err(WorkRefusal::AlreadyLeased {
                holder: who(3),
                expires_at_ms: deadline,
            })
        );
        assert_eq!(
            may_take(&leased, &who(2), &any, &unit_ref(&unit), deadline),
            Ok(())
        );
    }

    /// A finished unit and an abandoned one are both refused, and the refusal
    /// says which — `outcome: None` is "nobody ever reported".
    #[test]
    fn a_settled_unit_is_abandoned_and_the_outcome_says_which_kind() {
        let (ops, unit) = submitted("process:v1", JobRequirements::any(), None);
        let done = with(
            &ops,
            vec![
                op(3, 200, 0, &WorkAct::Lease(unit_ref(&unit))),
                op(3, 210, 1, &completion(&unit)),
            ],
        );
        let err = may_take(
            &done,
            &who(2),
            &offer_of(&["process:v1"], None, 4),
            &unit_ref(&unit),
            300_000,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                WorkRefusal::Abandoned {
                    attempts: 1,
                    outcome: Some(_)
                }
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("reached a verdict"), "{err}");

        // And the shape that gives the variant its name: three leases, all
        // lapsed, nobody ever reporting.
        let mut lapsing = ops.clone();
        let mut at = 200i64;
        for (seq, seed) in [(0u64, 2u8), (0, 3), (1, 2)] {
            lapsing.push(op(seed, at, seq, &WorkAct::Lease(unit_ref(&unit))));
            at += (LEASE_MS as i64) / 1_000 + 1;
        }
        let silent = may_take(
            &fold(&lapsing),
            &who(2),
            &offer_of(&["process:v1"], None, 4),
            &unit_ref(&unit),
            (at as u64) * 1_000,
        )
        .unwrap_err();
        assert_eq!(
            silent,
            WorkRefusal::Abandoned {
                attempts: 3,
                outcome: None
            },
            "nobody reported — the Expired-vs-Free distinction, kept"
        );
        assert!(
            silent.to_string().contains("nobody ever reported"),
            "{silent}"
        );
    }

    /// The donor's own budget, and the boundary. Failing input: a donor whose
    /// offer says `max_concurrent: 0` — it offered to run nothing, and taking
    /// one unit would be one more than it said.
    #[test]
    fn a_donor_at_its_offered_concurrency_refuses() {
        let (submit, a, b) = submission();
        let ops = vec![
            op(1, 100, 0, &submit),
            op(2, 200, 0, &WorkAct::Lease(unit_ref(&a))),
        ];
        let proj = fold(&ops);

        assert_eq!(
            may_take(
                &proj,
                &who(2),
                &offer_of(&["process:v1"], None, 1),
                &unit_ref(&b),
                200_000
            ),
            Err(WorkRefusal::Concurrency { held: 1, max: 1 })
        );
        assert_eq!(
            may_take(
                &proj,
                &who(2),
                &offer_of(&["process:v1"], None, 2),
                &unit_ref(&b),
                200_000
            ),
            Ok(())
        );
        assert_eq!(
            may_take(
                &proj,
                &who(3),
                &offer_of(&["process:v1"], None, 0),
                &unit_ref(&b),
                200_000
            ),
            Err(WorkRefusal::Concurrency { held: 0, max: 0 }),
            "a donor that offered nothing takes nothing"
        );
    }

    /// A revoked handoff and one past its TTL both refuse on the submitter's
    /// side — the consent is gone, which is the same absence as never having
    /// had it.
    #[test]
    fn a_revoked_or_expired_handoff_refuses_the_submitter_side() {
        let (ops, unit) = submitted("process:v1", JobRequirements::any(), None);
        let any = offer_of(&["process:v1"], None, 4);
        let revoked = with(
            &ops,
            vec![op(
                1,
                200,
                1,
                &WorkAct::Revoke(Revocation { handoff: handoff() }),
            )],
        );
        assert_eq!(
            may_take(&revoked, &who(2), &any, &unit_ref(&unit), 200_000),
            Err(WorkRefusal::NotAllowed {
                actor: who(2),
                side: GrantSide::Submitter,
            })
        );

        // The default TTL is four hours from the `Submit` at t=100s.
        let proj = fold(&ops);
        let past_ttl = 100_000 + crate::DEFAULT_TTL_SECS * 1_000;
        assert_eq!(
            may_take(&proj, &who(2), &any, &unit_ref(&unit), past_ttl - 1),
            Ok(())
        );
        assert!(may_take(&proj, &who(2), &any, &unit_ref(&unit), past_ttl).is_err());
    }

    /// A unit whose payload was edited after sealing is refused with the
    /// sentence that names the fix, before any consent or queue check.
    #[test]
    fn a_unit_whose_hash_does_not_cover_its_payload_is_refused_first() {
        let (ops, unit) = submitted("process:v1", JobRequirements::any(), None);
        let mut proj = fold(&ops);
        proj.handoffs
            .get_mut(&handoff())
            .expect("the handoff")
            .units
            .get_mut(&unit.unit_hash)
            .expect("the unit")
            .unit
            .payload = json!({ "argv": ["rm", "-rf", "/"] });

        // Even with a donor that would fail the KIND check too, the identity
        // refusal is the one reported: nothing later is about a body anybody
        // agreed to.
        let err = may_take(
            &proj,
            &who(2),
            &offer_of(&["ingest:v1"], None, 4),
            &unit_ref(&unit),
            100_000,
        )
        .unwrap_err();
        assert_eq!(err.id(), "payload-not-canonical");
        assert!(err.to_string().contains("seals as"), "{err}");
    }
}

// -----------------------------------------------------------------
// The host half's own tests
// -----------------------------------------------------------------

/// Separate from the module above because these are the only tests here that
/// TOUCH THE MACHINE — `PATH` and `/run/.containerenv` — and they exist only
/// under the feature that admits that I/O.
#[cfg(all(test, feature = "process"))]
mod host_tests {
    use super::*;
    use oicp_types::{JobKind, JobRequirements, JobUnit};

    fn unit_needing(preconditions: Vec<Precondition>) -> JobUnit {
        let payload = serde_json::json!({ "argv": ["true"] });
        JobUnit {
            unit_hash: crate::seal::unit_hash(
                &JobKind::parse("process:v1").expect("a kind"),
                &payload,
            )
            .expect("canonical"),
            kind: JobKind::parse("process:v1").expect("a kind"),
            payload,
            requirements: JobRequirements {
                preconditions,
                ..JobRequirements::any()
            },
            tenant: None,
        }
    }

    /// No preconditions is not a refusal.
    ///
    /// The accept case, so a decider that returned `Err` unconditionally — or
    /// one whose loop body never ran — cannot pass the tests below for free.
    #[test]
    fn a_unit_asking_nothing_of_the_host_is_admitted() {
        assert_eq!(host_satisfies(&unit_needing(vec![])), Ok(()));
    }

    /// A binary that is present passes; one that is absent is REFUSED BY NAME.
    ///
    /// Failing input: `Precondition::Binary("definitely-not-a-real-binary…")`.
    /// A decider that assumed an unfound binary was fine would lease the unit,
    /// run an argv whose interpreter does not exist, and report a failure
    /// about the SUBMITTER's code that is really a fact about this machine.
    #[test]
    fn an_absent_binary_is_refused_and_the_refusal_names_it() {
        // `sh` is the one binary a POSIX host running this test must have.
        assert_eq!(
            host_satisfies(&unit_needing(vec![Precondition::Binary("sh".into())])),
            Ok(()),
            "a binary that IS on PATH must not refuse, or the test below is vacuous"
        );

        let missing = Precondition::Binary("definitely-not-a-real-binary-9f2c".into());
        let err = host_satisfies(&unit_needing(vec![missing.clone()])).unwrap_err();
        assert_eq!(
            err,
            WorkRefusal::RequirementUnmet(UnmetRequirement::Precondition(missing))
        );
        assert!(
            err.to_string()
                .contains("definitely-not-a-real-binary-9f2c"),
            "the refusal must name the precondition, not just its kind: {err}"
        );
    }

    /// **The rule this function exists for (ARCH §18.3).** A precondition this
    /// build cannot EVALUATE is refused, never assumed met.
    ///
    /// Failing input: any of the three below. Each is a question about an
    /// agent runtime's state that a package crate deliberately cannot reach,
    /// and the tempting wrong answer is `true` — "I cannot check it, so it is
    /// probably fine". That donor returns a green about a machine that never
    /// met the unit's terms, which is the whole failure mode the four-verdict
    /// vocabulary exists to prevent.
    #[test]
    fn a_precondition_this_build_cannot_evaluate_is_refused_not_assumed() {
        for p in [
            Precondition::PortListening(9741),
            Precondition::SlotDecodes("primary".into()),
            Precondition::CorpusInstalled("sep".into()),
        ] {
            assert_eq!(
                host_satisfies(&unit_needing(vec![p.clone()])),
                Err(WorkRefusal::RequirementUnmet(
                    UnmetRequirement::Precondition(p.clone())
                )),
                "{p:?} is not evaluable here and must refuse, never pass"
            );
        }
    }

    /// A container precondition is answered from `/run/.containerenv`, and a
    /// name that is not this container's refuses.
    ///
    /// Deliberately asserts only the direction that holds on EITHER side of
    /// the boundary: a container nobody is in cannot be satisfied. Asserting
    /// the positive would pin the test to the toolbox and make it a fact about
    /// the runner rather than about the decider.
    #[test]
    fn a_container_this_process_is_not_in_refuses() {
        let p = Precondition::Container("not-a-container-that-exists-4b1a".into());
        assert_eq!(
            host_satisfies(&unit_needing(vec![p.clone()])),
            Err(WorkRefusal::RequirementUnmet(
                UnmetRequirement::Precondition(p)
            ))
        );
    }

    /// The FIRST unmet precondition is the one reported, and a later met one
    /// does not rescue it.
    ///
    /// Failing input: a loop that ORs instead of ANDs. `sh` is present, so an
    /// any-of decider would admit this unit and run it on a host missing the
    /// thing it actually asked for.
    #[test]
    fn one_unmet_precondition_refuses_the_whole_unit() {
        let missing = Precondition::Binary("definitely-not-a-real-binary-9f2c".into());
        let err = host_satisfies(&unit_needing(vec![
            Precondition::Binary("sh".into()),
            missing.clone(),
        ]))
        .unwrap_err();
        assert_eq!(
            err,
            WorkRefusal::RequirementUnmet(UnmetRequirement::Precondition(missing))
        );
    }
}
