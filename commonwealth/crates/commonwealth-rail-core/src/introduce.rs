// SPDX-License-Identifier: AGPL-3.0-or-later
//! Why a key is in the roster — an act the rail can READ and never acts on.
//!
//! # The gap this closes
//!
//! [`Roster`] binds a display name to signing keys and says nothing about how
//! a key got there. A key is in a ring because an operator typed `svrn ring
//! roster add`, and the reason lives in that operator's memory. Ask why
//! someone is in a ring and the only answer is somebody's recollection.
//!
//! Two halves close it and they are worthless apart. [`Introduce`] is the
//! act — a member the ring already claims vouches for a key, with a reason,
//! signed like every other op. [`Vouch`] is the roster row's record of the
//! introduction it was admitted on. Provenance with nothing to populate it is
//! a field nobody fills; an act with nowhere to land is a payload nobody
//! reads.
//!
//! # Three refusals shape this, and all three come from the code
//!
//! **An op never changes a roster.** `sovereign-mesh/src/ring_roster.rs`
//! states it: "The roster is written from here and is not reachable from the
//! rail at all. There is no roster route, so a deployed app cannot add a key
//! to the ring — including its own." An [`Introduce`] arriving from a peer
//! therefore moves nothing. It is evidence a human reads and acts on, and the
//! only writer of a roster is still `svrn ring roster add`.
//!
//! **The roster is not a function of the op set.** See [`Roster`]'s own docs:
//! an app that derived membership from the log would re-divide every past
//! expense the day a housemate joins, and the reference app has a test
//! pinning that it does not. So nothing here folds introductions into a
//! roster — [`trace`] reads a roster and reports, it never builds one.
//!
//! **[`admit`](crate::admit) learns no meaning.** `Introduce` is deliberately
//! NOT a [`RailAct`](crate::RailAct) variant: a variant would put a branch
//! for it inside admission, which is the rail deciding what an act means. It
//! is an ordinary opaque [`Payload`] on a `Record`, and admission carries it
//! the way it carries an expense. The only readers are a human, this module,
//! and the CLI that writes the roster.
//!
//! # Why resolution takes ADMITTED ops
//!
//! [`trace`] resolves against [`AdmittedOp`]s rather than journal lines, and
//! that is the whole reason there is no signature check in this file.
//! Admission has already refused every op whose signature did not verify and
//! every actor no roster row claims, so "exists, verifies, and was signed by
//! a member" is discharged by the fact that the op is IN the list. A second
//! verification here would be a second answer to the question the signature
//! exists to settle (ARCH §10.6).

use serde::{Deserialize, Serialize};

use crate::{AdmittedOp, OpId, Payload, PayloadError, Person, Roster};

/// One member vouching for a key, signed under their own.
///
/// Rides the rail as an ordinary [`Payload`] on a
/// [`RailAct::Record`](crate::RailAct::Record) — see the module docs on why
/// it is not a `RailAct` variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduce {
    /// The name the ring should call this key's owner. Checked against the
    /// roster row by [`trace`]: an introduction of *Alex* cannot be the
    /// warrant for a row that says *Bo*.
    pub person: Person,
    /// The hex node public key being vouched for.
    pub key: String,
    /// What the vouch rests on, in the writer's own words. Free text because
    /// the rail cannot judge a reason and must not pretend to — it is read by
    /// the person deciding whether to admit the key.
    pub reason: String,
}

impl Introduce {
    /// The payload's `kind`. A payload is an act and an act needs a name for
    /// what it is; [`PayloadError::NotAnObject`] already tells every app
    /// author to write one.
    pub const KIND: &'static str = "introduce";

    pub fn new(person: Person, key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            person,
            key: key.into(),
            reason: reason.into(),
        }
    }

    /// The canonical payload to sign.
    pub fn payload(&self) -> Result<Payload, PayloadError> {
        Payload::new(serde_json::json!({
            "kind": Self::KIND,
            "person": self.person.as_str(),
            "key": self.key,
            "reason": self.reason,
        }))
    }

    /// Read one back off an admitted op, or `None` for a payload that is not
    /// exactly this shape.
    ///
    /// The shape IS the act — `kind` plus the three fields — so an app free
    /// to write its own acts cannot have one of them mistaken for an
    /// introduction by accident, and a partially-formed one is `None` rather
    /// than an introduction with an empty field.
    pub fn from_payload(payload: &Payload) -> Option<Self> {
        let v = payload.as_value();
        if v.get("kind")?.as_str()? != Self::KIND {
            return None;
        }
        Some(Self {
            person: Person::from(v.get("person")?.as_str()?),
            key: v.get("key")?.as_str()?.to_string(),
            reason: v.get("reason")?.as_str()?.to_string(),
        })
    }
}

/// Why one key is in the roster: the introduction it was admitted on.
///
/// Named `Vouch` and not `Warrant`, which is what the campaign that asked for
/// it calls this, because `docs/internal/WARRANT_DESIGN.md` is minting
/// `pub struct Warrant` for a different concept in flight — a time-boxed,
/// attenuable claim on compute. Two types called `Warrant` in one workspace
/// would be one name for two essences (ARCH §10.6 on identity from essence).
///
/// `by` and `at` are a legible COPY of what the op already says, not a second
/// source of truth: [`trace`] refuses a row whose stored pair disagrees with
/// the op it names ([`VouchStatus::DisagreesWithTheOp`]). They are stored
/// because a journal can be sealed and compacted — the op goes away, and
/// "introduced by a key I can still name" is a better answer than silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vouch {
    /// The [`Introduce`] op this row was admitted on.
    pub op: OpId,
    /// The hex actor key that signed that op.
    pub by: String,
    /// That op's `ts_unix`.
    pub at: i64,
}

/// What a roster row's [`Vouch`] resolves to.
///
/// One enum rather than a two-level `Result`-shaped pair, so a renderer has
/// exactly one `match` and a new way for a row to be untraceable cannot be
/// added without every reader seeing it (ARCH §9 on closed sets).
///
/// Everything that is not [`Traced`](VouchStatus::Traced) counts against
/// `ra-introduction-is-traceable`, including [`Unknown`](VouchStatus::Unknown)
/// — a row with no vouch at all is not a failure of this rung (it is every
/// row written before it, and every founder who had nobody to vouch for
/// them), but it is not traceable either and must never be counted as if it
/// were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VouchStatus {
    /// The row carries no vouch. A roster written before this rung, or a
    /// founder: the first key in a ring is added by the person holding it,
    /// because there is nobody yet to vouch.
    Unknown,
    /// The op exists, admission verified it, it is an introduction, and it
    /// names this row.
    Traced {
        op: OpId,
        /// Who the roster says signed it.
        by: Person,
        /// The hex key that signed it.
        by_actor: String,
        at: i64,
        reason: String,
    },
    /// No admitted op with that id. Either this node does not hold it, or it
    /// held it and admission refused it — a bad signature, a tampered id, a
    /// signer no roster row claims. The two are one answer here on purpose:
    /// from the roster's side both mean *the evidence is not in front of me*.
    NotHeld { op: OpId },
    /// The op is admitted but carries no introduction — another app's act, a
    /// correction with no replacement, or a seal.
    NotAnIntroduction { op: OpId },
    /// An introduction, but of somebody else.
    NamesAnother {
        op: OpId,
        person: Person,
        key: String,
    },
    /// The introduction was voided by a later correction. The vouch was
    /// withdrawn, visibly and permanently — the row stands, and its warrant
    /// does not.
    Withdrawn { op: OpId },
    /// The introducer vouched for their own key. Circular: the op was only
    /// admissible because that key was already in the roster, so it is the
    /// key asserting its own membership. A second laptop introduced by the
    /// first is NOT this — those are two different keys.
    SelfVouch { op: OpId },
    /// The introducer's own row was admitted on a LATER introduction than
    /// this one, so at the moment they vouched the ring did not yet claim
    /// them. This is the "already claimed **at that time**" half, and the
    /// only ordering evidence a roster carries.
    IntroducerNotClaimedYet { op: OpId, by_actor: String },
    /// The row's stored `by`/`at` are not what the op says. The op is
    /// authoritative; the row is not silently corrected to match it
    /// (ARCH §18.3 — never substitute).
    DisagreesWithTheOp { op: OpId },
}

impl VouchStatus {
    /// Whether this row resolves — the numerator of
    /// `ra-introduction-is-traceable`. One definition, so the renderer, the
    /// CLI's refusal and any count agree.
    pub fn is_traced(&self) -> bool {
        matches!(self, Self::Traced { .. })
    }
}

impl std::fmt::Display for VouchStatus {
    /// A sentence a person can act on, because this is what a terminal prints
    /// under a roster row.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => write!(f, "warrant unknown — added before anyone had to say why"),
            Self::Traced {
                op, by, at, reason, ..
            } => write!(f, "introduced by {by}, op {op}, {at} — {reason}"),
            Self::NotHeld { op } => write!(
                f,
                "warrant unresolved — op {op} is not admitted on this node (not held, or refused)"
            ),
            Self::NotAnIntroduction { op } => {
                write!(f, "warrant unresolved — op {op} is not an introduction")
            }
            Self::NamesAnother { op, person, key } => write!(
                f,
                "warrant unresolved — op {op} introduces {person} ({key}), not this row"
            ),
            Self::Withdrawn { op } => {
                write!(f, "warrant withdrawn — op {op} was voided by a correction")
            }
            Self::SelfVouch { op } => write!(
                f,
                "warrant unresolved — op {op} vouches for the key that signed it"
            ),
            Self::IntroducerNotClaimedYet { op, by_actor } => write!(
                f,
                "warrant unresolved — {by_actor} signed op {op} before the ring claimed them"
            ),
            Self::DisagreesWithTheOp { op } => write!(
                f,
                "warrant unresolved — this row's signer and date are not what op {op} says"
            ),
        }
    }
}

/// Resolve one roster row against the admitted log.
///
/// The entry point for a reader: a row with no [`Vouch`] is
/// [`VouchStatus::Unknown`], and a row with one is resolved by [`trace_op`]
/// and then checked against the pair the row stored.
pub fn trace(roster: &Roster, admitted: &[AdmittedOp], person: &Person, key: &str) -> VouchStatus {
    let Some(vouch) = roster.vouch_for(key) else {
        return VouchStatus::Unknown;
    };
    let status = trace_op(roster, admitted, person, key, &vouch.op);
    let status = match &status {
        VouchStatus::Traced { by_actor, at, .. } if by_actor != &vouch.by || at != &vouch.at => {
            VouchStatus::DisagreesWithTheOp {
                op: vouch.op.clone(),
            }
        }
        _ => status,
    };
    // Why this row did or did not resolve, at the grain the answer is decided
    // (ARCH §9.1). The status is also RETURNED and rendered, but a row that
    // stops resolving after a correction lands is exactly the kind of change
    // an operator finds in a log rather than by re-running a command.
    tracing::debug!(
        %person,
        key,
        op = %vouch.op,
        traced = status.is_traced(),
        status = %status,
        "ring rail: roster warrant"
    );
    status
}

/// Resolve a CANDIDATE introduction: the checks, without a roster row to read
/// the op id from.
///
/// This is what `svrn ring roster add --on <op>` calls before it writes
/// anything, which is why it takes the op id rather than a [`Vouch`] — the
/// [`Vouch`] is MINTED from the op this returns, so the writer never asks an
/// operator for a signer or a date it can read off the signed act itself
/// (ARCH §18.1 — assert on what the subject cannot echo back).
pub fn trace_op(
    roster: &Roster,
    admitted: &[AdmittedOp],
    person: &Person,
    key: &str,
    op_id: &OpId,
) -> VouchStatus {
    let Some(op) = admitted.iter().find(|o| &o.id == op_id) else {
        return VouchStatus::NotHeld { op: op_id.clone() };
    };
    if op.voided {
        return VouchStatus::Withdrawn { op: op_id.clone() };
    }
    let intro = op
        .payload
        .as_ref()
        .and_then(Introduce::from_payload)
        .filter(|_| !op.voided);
    let Some(intro) = intro else {
        return VouchStatus::NotAnIntroduction { op: op_id.clone() };
    };
    if intro.key != key || &intro.person != person {
        return VouchStatus::NamesAnother {
            op: op_id.clone(),
            person: intro.person,
            key: intro.key,
        };
    }
    if op.actor == key {
        return VouchStatus::SelfVouch { op: op_id.clone() };
    }
    // "Claimed **at that time**", with the only ordering evidence a roster
    // holds: if the introducer's OWN row was admitted on a later
    // introduction, they were not yet a member when they vouched. A
    // introducer with no vouch is a founder and was claimed from the start.
    if let Some(theirs) = roster.vouch_for(&op.actor) {
        if theirs.at > op.ts_unix {
            return VouchStatus::IntroducerNotClaimedYet {
                op: op_id.clone(),
                by_actor: op.actor.clone(),
            };
        }
    }
    VouchStatus::Traced {
        op: op_id.clone(),
        by: op.person.clone(),
        by_actor: op.actor.clone(),
        at: op.ts_unix,
        reason: intro.reason,
    }
}
