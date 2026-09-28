// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`WorkRefusal`] — why a donor may not take a unit. The one predicate over
//! the rail's state, `may_take`, and the host's half, `host_satisfies`, stay
//! in `commonwealth_work::refusal`, which re-exports this vocabulary at its
//! historical path (pb-work-doors).

use kernel_types::quality::Precondition;
use kernel_types::{ActorKey, Judgement};
use serde::{Deserialize, Serialize};

use crate::{Isolation, JobKind};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
/// [`JobRequirements`](crate::JobRequirements) has four halves and the
/// order's spelling of the refusal — `RequirementUnmet(Precondition)` — can
/// only carry one of them. This is that spelling with the other three beside
/// it, so `repo_rev`, `os` and `arch` refusals are as typed and as renderable
/// as a missing binary; the variant count of [`WorkRefusal`] stays at ten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    Precondition(#[serde(with = "crate::job::precondition_label")] Precondition),
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
// Serde since pb-work-doors: cw-rails' refusals door answers `may_take`'s
// verdicts typed, so a submitter renders the rail's own refusal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkRefusal {
    /// This donor's offer does not name this kind at all.
    KindNotOffered { kind: JobKind },
    /// The donor offers the same kind at a different revision. A DIFFERENT
    /// refusal from [`KindNotOffered`](WorkRefusal::KindNotOffered) on
    /// purpose: one means "wrong machine", the other means "update one of
    /// us", and `JobKind::is_skew_of` is the one place the difference is
    /// decided.
    VersionSkew { wanted: JobKind, offered: JobKind },
    /// Nobody consented. See [`GrantSide`] for which half.
    NotAllowed { actor: ActorKey, side: GrantSide },
    /// The donor cannot isolate the unit as strongly as its SUBMITTER asked
    /// for — `JobRequirements::isolation` against `WorkOffer.isolation`,
    /// compared by `may_take` since 2026-09-10. It had no producer at all
    /// before that: this variant, the offer field and `Isolation::covers` all
    /// existed and nothing joined them.
    ///
    /// Not to be confused with the donor's own floor, which decides what this
    /// node may OFFER (`JobExecutorRegistry::offerable`) and refuses earlier
    /// and for a different reason.
    IsolationBelow {
        required: Isolation,
        offered: Isolation,
    },
    /// The host does not satisfy one of the unit's requirements.
    RequirementUnmet(UnmetRequirement),
    /// Somebody else's lease is still live. Not an error — the normal outcome
    /// of two donors reading one queue.
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
    Abandoned {
        attempts: u32,
        outcome: Option<Judgement>,
    },
    /// The unit's payload is not something that can be run — its declared
    /// identity does not cover its body, or the executor's schema refuses it.
    /// `detail` carries the sentence that names the fix.
    PayloadNotCanonical { detail: String },
    /// The donor is already holding as many leases as its offer allows.
    Concurrency { held: u32, max: u32 },
    /// The donor's operator is at the keyboard and its offer says to stand
    /// down. Constructed by the donor loop — see the module docs.
    Yielding,
}

// Written out rather than derived: this leaf takes no `thiserror`
// (Cargo.toml's dependency budget). The sentences are the ones the derive
// carried in `commonwealth-work` before pb-work-doors moved the type here.
impl std::fmt::Display for WorkRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkRefusal::KindNotOffered { kind } => {
                write!(f, "this donor does not offer `{kind}`")
            }
            WorkRefusal::VersionSkew { wanted, offered } => write!(
                f,
                "this donor offers `{offered}` and the unit is `{wanted}` — same kind, different contract revision"
            ),
            WorkRefusal::NotAllowed { actor, side } => write!(
                f,
                "`{actor}` is not inside the {} half of the grant",
                side.id()
            ),
            WorkRefusal::IsolationBelow { required, offered } => write!(
                f,
                "this unit needs `{required:?}` isolation and this donor offers `{offered:?}`"
            ),
            WorkRefusal::RequirementUnmet(unmet) => write!(f, "{unmet}"),
            WorkRefusal::AlreadyLeased {
                holder,
                expires_at_ms,
            } => write!(
                f,
                "`{holder}` holds a live lease on this unit until {expires_at_ms}ms"
            ),
            WorkRefusal::Abandoned { attempts, outcome } => {
                f.write_str(&abandoned_sentence(*attempts, outcome))
            }
            WorkRefusal::PayloadNotCanonical { detail } => {
                write!(f, "this unit's payload cannot be run: {detail}")
            }
            WorkRefusal::Concurrency { held, max } => write!(
                f,
                "this donor holds {held} of the {max} concurrent leases it offered"
            ),
            WorkRefusal::Yielding => f.write_str("this donor is yielding to foreground work"),
        }
    }
}

impl std::error::Error for WorkRefusal {}

/// The sentence for [`WorkRefusal::Abandoned`], which branches on whether
/// anybody ever reported. Free function because it was `thiserror`'s `#[error]`
/// argument, which takes a format expression and not a match.
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
