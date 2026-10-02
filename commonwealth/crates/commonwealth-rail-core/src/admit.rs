// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`admit`] — turn a bag of journal lines into the one order every node
//! applies them in, and say what is missing.
//!
//! # The cut this module is one half of
//!
//! There were thirteen ways a ring op could fail to count, and they were one
//! enum. Eight of them — a torn line, a bad signature, a stranger's key, a
//! rewritten id, a hole in a sequence, an equivocated sequence number, a
//! correction pointing at nothing, a line from a newer build — are about
//! **delivery and authenticity**, and not one of them knows what an expense
//! is. The other five were about money.
//!
//! That was one type doing two jobs, and the tell was that the money half
//! could not move: an app that lends drills instead of splitting groceries
//! needs every rule in this file and none of the other five. So the rail
//! keeps this half, in Rust, where the signature checking and the convergence
//! property live; the app keeps its own half, in its own vocabulary, over the
//! payloads this function hands back.
//!
//! # The one property this file exists to have
//!
//! **The result is a function of the op SET.** Not of arrival order, not of
//! file position, not of the transport's `timestamp`. Nineteen laptops gossip
//! the same ops in nineteen different orders; if admission depended on order,
//! two housemates would read different numbers off the same journal and the
//! whole thing would be worse than the spreadsheet it replaces.
//!
//! Everything below that looks fussy is in service of that one property:
//!
//! | Rule | Without it |
//! |---|---|
//! | dedupe by re-derived [`OpId`] | a replayed op is counted twice |
//! | total order `(ts_unix, actor, seq, id)` | tie-breaking differs per node, and one actor's own burst inside a second folds against its causality |
//! | void set built from ALL corrections at once | a correction that arrives before its target does nothing on one node and something on another |
//! | corrections never resurrect | un-voiding depends on which correction is "last" |
//! | gaps sorted before returning | the *report* differs even when the payloads agree |
//!
//! Proving it here rather than over balances makes it a **stronger** claim
//! and a cheaper one: the exhaustive permutation test no longer has to pick a
//! tenant to be true about.
//!
//! # Why the void set is in the rail and not in the app
//!
//! "This earlier act was wrong, and it never comes back" is not an expense
//! rule. A tool-lending board needs it the moment somebody writes *I returned
//! the drill* and then *no I didn't*. Leaving it to the app means every
//! author re-derives the one rule that makes the void set commutative — build
//! it from every correction at once, never walk for liveness — and the ones
//! who get it wrong get an app that converges in testing and diverges in a
//! house.
//!
//! # And the property it refuses to fake
//!
//! An admission over ops that have not all arrived is an admission over a
//! subset. It returns [`RailGap`]s rather than a bare list, because a rail
//! that cannot say "I may be missing something" lets an app state a wrong
//! total with complete confidence, which is the failure ARCH §18.3 names.

use std::collections::{BTreeMap, BTreeSet};

use oplog_types::{Op, OpId};

use crate::payload::Payload;
use crate::{Person, RailAct, RingVerifier, Roster, SignedOp};

/// Something the rail could not account for. Never fatal, always reported.
///
/// Every variant is about **delivery or authenticity**. Nothing here reads
/// inside a payload, which is why this enum is the same eight cases for an
/// expense book and a tool-lending board. An app's own reasons for refusing
/// an act are the app's to name, over the payloads [`admit`] returns.
///
/// Ordered and sorted before admission returns, so two nodes holding the same
/// ops produce a byte-identical report and not merely an equal set of acts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(tag = "gap", rename_all = "snake_case")]
pub enum RailGap {
    /// A journal line this build could not parse. From
    /// [`SkippedLine::Malformed`](oplog_types::SkippedLine) — a torn
    /// write, or a payload that has no canonical form (see
    /// [`Payload`](crate::Payload)).
    MalformedLine { line: u64, error: String },
    /// A journal line written by a NEWER build. Reading it would be guessing;
    /// counting the rest and saying nothing would be worse (ARCH §18.3), so
    /// an un-upgraded node reports that its answer covers a strict subset.
    NewerVersionLine { line: u64, v: u32 },
    /// The signature does not verify under the public key the line names.
    /// The op is not admitted.
    BadSignature { id: OpId, actor: String },
    /// The signature verifies, but no one in the roster signs with that key.
    /// Self-certifying is not the same as being a member. Not admitted.
    UnknownSigner { id: OpId, actor: String },
    /// The `id` on the line is not the id its content derives. Admission uses
    /// the derived id, so this changes no outcome — it is reported because a
    /// peer writing lines whose id has been rewritten is a fact worth seeing.
    TamperedId { claimed: OpId, derived: OpId },
    /// This actor's ops jump over a sequence number. Something they wrote has
    /// not reached us — the one condition that distinguishes "nothing
    /// happened" from "it never arrived."
    ///
    /// Counted from that actor's sealed floor, so what a
    /// [`Seal`](crate::RailAct::Seal) retired is absent by agreement rather
    /// than missing.
    SequenceHole { actor: String, missing: u64 },
    /// One actor used one sequence number for two different ops. Equivocation
    /// or a lost counter after a restart; either way both ops are excluded,
    /// because picking one would be picking an answer out of the air.
    SequenceFork {
        actor: String,
        seq: u64,
        ids: Vec<OpId>,
    },
    /// A correction naming an op we do not hold. The correction itself folds;
    /// its citation resolves to nothing here. Harmless to the order (the void
    /// is recorded and applies the moment the target arrives), and it hides
    /// no act from the fold — if the target is real and missing, its author's
    /// run reports the [`SequenceHole`], and if that author is wholly absent
    /// this gap names the exact op to ask for. It is the ask, not silence,
    /// and a typo'd citation must not flip every node's completeness bit
    /// forever (ROOT_CAUSE_FIXES A2).
    DanglingCorrection { by: OpId, missing: OpId },
    /// The signer is bound in the record but held no standing at this act's
    /// position — written while removed, or before their own
    /// [`Admit`](crate::RailAct::Admit) in the order. The act counts for
    /// nothing and is named rather than dropped: it may be real data (voiding
    /// the [`Remove`](crate::RailAct::Remove) admits it back — leg 3 of
    /// `ra-membership-is-order-free`).
    NotAMember { id: OpId, actor: String },
}

/// What a gap means for the fold's answer — the one classification (ARCH
/// §10.6), asked the question `is_complete` exists to answer: **can this gap
/// be hiding acts that belong in the fold?**
///
/// The line matters more than it looks: a refusal is a refusal, never an
/// absence (ARCH §18.3), and an absence is never silence — but "the fold is
/// missing data" and "the record contradicts itself" are different facts and
/// one typo'd [`Correct`](crate::RailAct::Correct) used to masquerade as the
/// former forever. The split is by coverage, not by scariness: a forged or
/// forked act is Absence precisely because it may be REAL data the fold
/// refused (the acts are real but they are a subset), while a tampered id or
/// a dangling citation is Contradiction because every act that could be
/// folded is folded — the gap is a fact about a claim the record makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapClass {
    /// The fold may lack acts that belong in it — an unreadable line, a
    /// refused or unattributable act (which may be real), a forked-out seq,
    /// a seq that never arrived. [`Admission::is_complete`] is false exactly
    /// on these.
    Absence,
    /// Every act that could be folded is folded: the gap is a claim the
    /// record makes that nothing here can stand behind — an id its content
    /// does not derive, or a correction citing an op nobody holds. Named
    /// beside a complete answer.
    Contradiction,
}

impl RailGap {
    /// The one classification of what a gap means (ARCH §10.6). Consumers
    /// ask their own question of it — completeness is `class() ==
    /// Absence`; `ring checkpoint --verify` refuses a document on the gaps
    /// whose acts cannot stand — but there is one answer per kind here.
    pub fn class(&self) -> GapClass {
        match self {
            Self::MalformedLine { .. }
            | Self::NewerVersionLine { .. }
            | Self::BadSignature { .. }
            | Self::UnknownSigner { .. }
            | Self::NotAMember { .. }
            | Self::SequenceFork { .. }
            | Self::SequenceHole { .. } => GapClass::Absence,
            Self::TamperedId { .. } | Self::DanglingCorrection { .. } => GapClass::Contradiction,
        }
    }
}

/// One sentence a person can act on.
///
/// **The one renderer** (ARCH §10.6). A gap is shown in three places — the
/// `svrn ring log` table, a ring app's own page, and the refusal the append
/// door returns — and each writing its own prose is how three of them end up
/// saying different things about the same condition. Before this existed the
/// door returned a serde dump (`{"gap":"bad_signature",…}`) straight at a
/// housemate.
impl std::fmt::Display for RailGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedLine { line, .. } => {
                write!(f, "journal line {line} could not be read")
            }
            Self::NewerVersionLine { line, .. } => write!(
                f,
                "journal line {line} was written by a newer build — upgrade to read it"
            ),
            Self::BadSignature { id, .. } => write!(
                f,
                "an op whose signature does not verify ({})",
                crate::short_id(id.as_str())
            ),
            Self::NotAMember { actor, .. } => write!(
                f,
                "an op by actor {actor} was written while they held no standing in the ring"
            ),
            Self::UnknownSigner { actor, .. } => write!(
                f,
                "an op signed by {}… — nobody in the roster claims that key",
                crate::actor_prefix(actor)
            ),
            Self::TamperedId { claimed, .. } => write!(
                f,
                "a journal line whose id ({}) does not match its content",
                crate::short_id(claimed.as_str())
            ),
            Self::SequenceHole { actor, missing } => write!(
                f,
                "an op from {}… has not reached this node yet (#{missing})",
                crate::actor_prefix(actor)
            ),
            Self::SequenceFork { actor, seq, .. } => write!(
                f,
                "{}… used one sequence number twice (#{seq}) — both ops are excluded",
                crate::actor_prefix(actor)
            ),
            Self::DanglingCorrection { missing, .. } => write!(
                f,
                "a correction of {}, which this node does not hold",
                crate::short_id(missing.as_str())
            ),
        }
    }
}

/// One op that passed admission, ready for an app's reducer.
///
/// The rail has already done everything generic to it: the id is re-derived
/// from content, the signature verified, the signer looked up in the roster,
/// and the position in `Admission::ops` is the total order every node agrees
/// on. What is left — what the payload *means* — is the app's.
/// `Deserialize` as well as `Serialize` since cw-lift 5d, and the pair is the
/// point: `GET /v1/rail/log` ships `Admission::ops` verbatim, and a client that
/// wants to FOLD that answer — `svrn job status` is the first — has to be able
/// to read back exactly what this type wrote. The alternative was a second
/// struct in the CLI mirroring these fields, which is one wire shape with two
/// spellings and drifts the day a field is added here (ARCH §10.6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdmittedOp {
    /// Content-derived id. This is what a correction names, and what an app
    /// hands back to [`RailAct::Correct`](crate::RailAct::Correct).
    pub id: OpId,
    /// The signing public key — the only field on the line a writer cannot
    /// forge for someone else (ARCH §18.1).
    pub actor: String,
    /// Who the roster says that key is. Present because admission already
    /// refused every key the roster does not know, so an app never has to
    /// render a balance against `node-44a1b3e8`.
    pub person: Person,
    pub seq: u64,
    pub ts_unix: i64,
    /// What this op voids, when it is a correction. The void is **already
    /// applied** — carried so an app can say *what changed*, never so it can
    /// re-derive the void set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects: Option<OpId>,
    /// `true` when a correction voided this op. It stays in the list so an
    /// app can render history, and the SDK's `fold` skips it. An app that
    /// walks `ops` itself instead of folding will double-count these — which
    /// is why the SDK ships the fold.
    pub voided: bool,
    /// The app's act. `None` is a correction that only voids, and states no
    /// replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
    /// [`SignedOp::on_behalf_of`](crate::SignedOp::on_behalf_of), carried
    /// through unchanged. Carried and not interpreted: admission does not
    /// look it up, does not refuse on it, and `person` still names the key's
    /// owner. It is here because `admit` is the only place an `AdmittedOp` is
    /// built, so a reader that never sees the signed line — the log route,
    /// and every app behind it — could not otherwise reach the field at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
}

impl AdmittedOp {
    /// Whether an app's reducer should see this op. The one definition of
    /// "surviving", so the SDK's fold and any Rust caller agree.
    pub fn applies(&self) -> bool {
        !self.voided && self.payload.is_some()
    }
}

/// What admission produced: the acts in their agreed order, and the honest
/// account of what could not be read.
///
/// `Deserialize` because the answer crosses the sync doors as this type: a
/// caller that dialled `/v1/rail/admit` reads back the same struct the
/// serving side admitted from, not a second spelling of it (ARCH §10.6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Admission {
    /// Every admitted op in the total order `(ts_unix, actor, seq, id)`, voided
    /// ones included and marked. An app folds this; it never sorts it.
    pub ops: Vec<AdmittedOp>,
    /// Everything the rail could not account for, sorted and deduplicated.
    pub gaps: Vec<RailGap>,
    /// How many journal lines this node holds, including the ones that did
    /// not survive admission. `held - ops.len()` is what was refused.
    pub held: usize,
    /// Where each actor's history starts, per the seals this admission
    /// AUTHENTICATED. Empty when nothing is sealed, and an actor absent from
    /// it starts at zero.
    ///
    /// Reported rather than kept private because it is the one input that
    /// stops a gap being raised, so a reader asking why this answer claims
    /// completeness cannot get there any other way. It is also what
    /// `commonwealth_rail::RingJournal::compact` deletes by: the floor a prune
    /// trusts has to be the floor admission trusted, or the destructive path
    /// gets its own second reading of the seals (ARCH §10.6, §18.3).
    pub floors: crate::sync::Floors,
    /// The fold's own conclusion about who is in: the one `membership`
    /// walk, carried out rather than re-derivable, so a reader asking the
    /// membership question cannot reach a second answer (ARCH §10.6). The
    /// seed, the act-admitted bindings and standing all live here.
    ///
    /// `None` only where the answer was rebuilt from a wire shape that
    /// predates the field — absence reported, never a default walk
    /// (ARCH §18.3).
    pub membership: Option<crate::membership::Membership>,
}

impl Admission {
    /// Whether this answer covers every act that could be in it. `false` is
    /// the signal a UI must not hide: the acts are real but they are a
    /// subset — a gap of [`GapClass::Absence`] can be hiding acts that
    /// belong in the fold. [`GapClass::Contradiction`] gaps do not flip it:
    /// they are named beside a complete answer (a tampered id changes no
    /// outcome; a dangling citation is the ask, not a hole).
    pub fn is_complete(&self) -> bool {
        !self.gaps.iter().any(|g| g.class() == GapClass::Absence)
    }

    /// The ops an app's reducer should apply, in order. The one definition
    /// (ARCH §10.6) — the JS SDK's `ring.fold` is this same filter.
    pub fn applied(&self) -> impl Iterator<Item = &AdmittedOp> {
        self.ops.iter().filter(|o| o.applies())
    }
}

// ── admission ────────────────────────────────────────────────

/// One op that passed the signature and roster checks, with the id the rail
/// actually uses.
struct Candidate<'a> {
    id: OpId,
    person: Person,
    op: &'a Op<SignedOp>,
}

/// Turn a bag of journal lines into the order every node applies them in.
///
/// `roster`, `namespace` and `verifier` are *parameters*, never folded state.
/// The namespace is bound into every signature, so it decides admission; the
/// verifier says whether a signature is real; the roster says which keys are
/// members. None of the three is ever handed to the app's reducer as anything
/// but the `person` on an already-admitted op — which is what keeps an app's
/// arithmetic a function of the op set even as people join and leave (pinned
/// by a test in the reference app).
///
/// The verifier is named rather than defaulted: which scheme judged these
/// signatures is a fact about the answer, and one that has to be greppable at
/// every call site instead of inherited from whatever the fold happened to be
/// compiled with. [`Ed25519Verifier`](crate::Ed25519Verifier) is the shipped
/// one.
pub fn admit(
    ops: &[Op<SignedOp>],
    skipped: &[oplog_types::SkippedLine],
    roster: &Roster,
    namespace: &str,
    verifier: &dyn RingVerifier,
) -> Admission {
    use oplog_types::SkippedLine;

    let mut gaps: Vec<RailGap> = skipped
        .iter()
        .map(|s| match s {
            SkippedLine::Malformed { line, error } => RailGap::MalformedLine {
                line: *line,
                error: error.clone(),
            },
            SkippedLine::NewerVersion { line, v } => {
                RailGap::NewerVersionLine { line: *line, v: *v }
            }
        })
        .collect();

    // ── signature ───────────────────────────────────────────
    let mut authentic: BTreeMap<OpId, &Op<SignedOp>> = BTreeMap::new();
    for op in ops {
        let derived = derived_id(op);
        if derived != op.id {
            gaps.push(RailGap::TamperedId {
                claimed: op.id.clone(),
                derived: derived.clone(),
            });
        }
        let body = body_json(
            &op.kind.act,
            op.kind.on_behalf_of.as_deref(),
            op.kind.view.as_ref(),
        );
        // Whatever the verifier cannot vouch for is a gap, never an act. A
        // `false` here is a refusal and is reported as one — there is no
        // answer that means "could not tell, carry on" (ARCH §18.3).
        if !verifier.verify(
            &op.actor,
            namespace,
            op.ts_unix,
            op.kind.seq,
            &body,
            &op.kind.sig,
        ) {
            gaps.push(RailGap::BadSignature {
                id: derived,
                actor: op.actor.clone(),
            });
            continue;
        }
        // Dedupe: the same op reaching us twice is the normal case under
        // gossip, not an anomaly.
        authentic.insert(derived, op);
    }

    // ── membership: the seed plus two act kinds, one function ─────────
    //
    // `roster` is the SEED (RING_APPLICATIONS.md "Amendment 2026-09-18"):
    // what is in the ring is decided by `membership`, and an act resolves
    // through the bindings its own record carries — so key churn cannot
    // re-flip the past (leg 4 of `ra-membership-is-order-free`).
    let m = crate::membership::membership(authentic.values().copied(), roster);

    let mut admitted: BTreeMap<OpId, Candidate<'_>> = BTreeMap::new();
    for (id, op) in authentic {
        let Some(person) = m.person_for(&op.actor) else {
            gaps.push(RailGap::UnknownSigner {
                id,
                actor: op.actor.clone(),
            });
            continue;
        };
        if !m.counted.contains(&id) && !m.voided.contains(&id) {
            gaps.push(RailGap::NotAMember {
                id,
                actor: op.actor.clone(),
            });
            continue;
        }
        admitted.insert(
            id.clone(),
            Candidate {
                id,
                person: person.clone(),
                op,
            },
        );
    }

    // ── per-actor sequence audit ─────────────────────────────
    let mut seqs: BTreeMap<&str, BTreeMap<u64, Vec<OpId>>> = BTreeMap::new();
    for a in admitted.values() {
        seqs.entry(a.op.actor.as_str())
            .or_default()
            .entry(a.op.kind.seq)
            .or_default()
            .push(a.id.clone());
    }
    let mut forked: BTreeSet<OpId> = BTreeSet::new();
    for (actor, by_seq) in &seqs {
        for (seq, ids) in by_seq {
            if ids.len() > 1 {
                let mut ids = ids.clone();
                ids.sort();
                gaps.push(RailGap::SequenceFork {
                    actor: (*actor).to_string(),
                    seq: *seq,
                    ids: ids.clone(),
                });
                forked.extend(ids);
            }
        }
    }
    admitted.retain(|id, _| !forked.contains(id));

    // ── the void set: commutative, and it never resurrects ───
    //
    // The set itself is computed in ONE place — `membership`, which applies
    // it before its walk so a correction that lands late still drops what it
    // targets and everything that target admitted. This pass only NAMES the
    // corrections whose target nobody here holds: the ask, never a hole
    // (GapClass::Contradiction).
    // Cloned, not moved: the whole walk — voided set included — is carried
    // out on `Admission` below, so the membership a reader sees is the one
    // this fold applied.
    let voided = m.voided.clone();
    for a in admitted.values() {
        if let RailAct::Correct { corrects, .. } = &a.op.kind.act {
            if !admitted.contains_key(corrects) {
                gaps.push(RailGap::DanglingCorrection {
                    by: a.id.clone(),
                    missing: corrects.clone(),
                });
            }
        }
    }

    // Holes are audited from each actor's SEALED FLOOR, not from zero, or a
    // node that has retired what a seal covers reports one hole per retired
    // op — permanently, to every housemate, while being in perfect health.
    // That report is what made compaction indistinguishable from breakage.
    //
    // The floors come from ops that survived BOTH the signature/roster checks
    // above and the fork exclusion just now, minus what the void set just
    // retired: a seal the rail refused, one it cannot choose between, or one
    // the log itself corrected retires nothing. Suppressing a hole is a claim
    // of completeness, and a claim of completeness may only rest on an act the
    // rail could actually authenticate (ARCH §18.3).
    let floors = crate::sync::sealed_floors(admitted.values().map(|c| c.op), &voided);
    for (actor, by_seq) in &seqs {
        let floor = floors.get(*actor).copied().unwrap_or(0);
        let highest = by_seq
            .keys()
            .copied()
            .next_back()
            .expect("seqs only holds actors that wrote");
        for n in floor..=highest {
            if !by_seq.contains_key(&n) {
                gaps.push(RailGap::SequenceHole {
                    actor: (*actor).to_string(),
                    missing: n,
                });
            }
        }
    }

    // ── the content-derived total order ──────────────────────
    //
    // `seq` sits between the actor and the id, and it is not decoration.
    // `ts_unix` is SECOND resolution, so an actor writing twice inside one
    // second used to be ordered by content hash — arbitrary, and arbitrary
    // against its own causality. Observed 2026-09-09 on the work plane: a
    // `Submit` and the `Lease` + `Complete` that answered it landed in the
    // same second, folded lease-then-complete-then-submit, and both of the
    // later acts were reported `unreadable` ("no admitted submission opened
    // this handoff") — so a unit ran TWICE, once for the discarded pair and
    // once for the retry. The rail already holds each actor's `seq` and
    // already refuses a fork on it (`SequenceFork` above), so ordering one
    // actor's own acts by anything else discards information this function
    // has verified (ARCH §7.5 — order from essence, not from an address).
    //
    // It stays DETERMINISTIC: `seq` is on the wire and every node reads the
    // same value, and the id remains the last term so the comparator is total
    // even for a pair the fork check somehow let through. Only within-actor,
    // within-second order changes.
    let mut order: Vec<&Candidate<'_>> = admitted.values().collect();
    order.sort_by(|x, y| {
        (x.op.ts_unix, &x.op.actor, x.op.kind.seq, &x.id).cmp(&(
            y.op.ts_unix,
            &y.op.actor,
            y.op.kind.seq,
            &y.id,
        ))
    });

    let out: Vec<AdmittedOp> = order
        .into_iter()
        .map(|a| {
            let (corrects, payload) = match &a.op.kind.act {
                RailAct::Record { payload } => (None, Some(payload.clone())),
                RailAct::Correct {
                    corrects,
                    replacement,
                } => (Some(corrects.clone()), replacement.clone()),
                // A seal is delivery, not meaning: it voids nothing and
                // carries nothing, so `applies()` is false and no reducer
                // sees it. Admit/Remove carry meaning to the MEMBERSHIP
                // function and never to an app (no-re-division).
                RailAct::Seal | RailAct::Admit { .. } | RailAct::Remove { .. } => (None, None),
            };
            AdmittedOp {
                id: a.id.clone(),
                actor: a.op.actor.clone(),
                person: a.person.clone(),
                seq: a.op.kind.seq,
                ts_unix: a.op.ts_unix,
                corrects,
                voided: voided.contains(&a.id),
                payload,
                on_behalf_of: a.op.kind.on_behalf_of.clone(),
            }
        })
        .collect();

    gaps.sort();
    gaps.dedup();

    let applied = out.iter().filter(|o| o.applies()).count();
    tracing::debug!(
        namespace,
        verifier = verifier.name(),
        held = ops.len(),
        // The one input that stops a gap being REPORTED, so it belongs on the
        // event a reader already looks at when asking why a node claims
        // completeness (ARCH §9.1). `{}` when nothing is sealed.
        sealed = ?floors,
        admitted = out.len(),
        applied,
        voided = voided.len(),
        gaps = gaps.len(),
        "ring rail: admission"
    );
    if !gaps.is_empty() {
        // Louder than the summary above on purpose: an admission with gaps
        // covers a subset, and it is the one thing about this answer an
        // operator reading logs needs to see without turning debug on.
        tracing::warn!(
            namespace,
            gaps = gaps.len(),
            first = ?gaps.first(),
            "ring rail: this answer covers a subset — the rail could not account for every op"
        );
    }

    Admission {
        ops: out,
        gaps,
        held: ops.len(),
        floors,
        membership: Some(m),
    }
}

/// Re-derive the op's id from its content, ignoring the one on the line.
///
/// Identity from essence (ARCH §7.5). A rewritten `id` field therefore cannot
/// make an op impersonate another op's correction target; it just gets
/// reported.
pub(crate) fn derived_id(op: &Op<SignedOp>) -> OpId {
    Op::new(op.kind.clone(), op.ts_unix, op.actor.clone()).id
}

/// The exact bytes the signature covers — the act, in declaration order, with
/// its payload canonical (see [`Payload`](crate::Payload)), followed by
/// `on_behalf_of` when the writer stated one, followed by `view` when the
/// writer committed to one.
///
/// The name is inside the signature because a stamp a peer could strip or
/// rewrite in flight would attribute an act to whoever last handled it. The
/// view is inside for the same reason one field later: a rewritten view
/// would make every act claim whichever history suited the carrier. Both are
/// LAST-in-order and omitted when `None`, so the bytes for an act with
/// neither are byte-identical to what this function returned before the
/// fields existed and every op already on every replica verifies unchanged.
pub fn body_json(
    act: &RailAct,
    on_behalf_of: Option<&str>,
    view: Option<&crate::Digest>,
) -> String {
    // A borrowing mirror of `SignedOp`'s signed half rather than a second
    // spelling of the rule: the act flattens in exactly as it serialises
    // alone, and the two added fields sit after it.
    #[derive(serde::Serialize)]
    struct Body<'a> {
        #[serde(flatten)]
        act: &'a RailAct,
        #[serde(skip_serializing_if = "Option::is_none")]
        on_behalf_of: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        view: Option<&'a crate::Digest>,
    }
    serde_json::to_string(&Body {
        act,
        on_behalf_of,
        view,
    })
    // A `RailAct` failing to serialise is unreachable; silent `""` would be
    // catastrophic — empty bytes on the signature path (ROOT_CAUSE_FIXES C4,
    // substitution-gate's named target).
    .expect("a RailAct serialises — signing empty bytes is the named catastrophe")
}
