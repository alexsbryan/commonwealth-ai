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
//! | total order `(ts_unix, actor, id)` | tie-breaking differs per node |
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

use oplog::{Op, OpId};

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
    /// [`SkippedLine::Malformed`](oplog::SkippedLine) — a torn
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
    SequenceHole { actor: String, missing: u64 },
    /// One actor used one sequence number for two different ops. Equivocation
    /// or a lost counter after a restart; either way both ops are excluded,
    /// because picking one would be picking an answer out of the air.
    SequenceFork {
        actor: String,
        seq: u64,
        ids: Vec<OpId>,
    },
    /// A correction naming an op we do not hold. Harmless to the order (the
    /// void is recorded and applies the moment the target arrives), but it
    /// means we are missing an op that may itself carry something.
    DanglingCorrection { by: OpId, missing: OpId },
}

impl RailGap {
    /// Shorten an op id for a sentence. The full 22 characters are in the
    /// JSON; a person reading a line needs enough to match it, not all of it.
    fn short(id: &OpId) -> String {
        let s = id.as_str();
        if s.len() > 12 {
            format!("{}…", &s[..12])
        } else {
            s.to_string()
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
                Self::short(id)
            ),
            Self::UnknownSigner { actor, .. } => write!(
                f,
                "an op signed by {}… — nobody in the roster claims that key",
                &actor[..actor.len().min(12)]
            ),
            Self::TamperedId { claimed, .. } => write!(
                f,
                "a journal line whose id ({}) does not match its content",
                Self::short(claimed)
            ),
            Self::SequenceHole { actor, missing } => write!(
                f,
                "an op from {}… has not reached this node yet (#{missing})",
                &actor[..actor.len().min(12)]
            ),
            Self::SequenceFork { actor, seq, .. } => write!(
                f,
                "{}… used one sequence number twice (#{seq}) — both ops are excluded",
                &actor[..actor.len().min(12)]
            ),
            Self::DanglingCorrection { missing, .. } => write!(
                f,
                "a correction of {}, which this node does not hold",
                Self::short(missing)
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corrects: Option<OpId>,
    /// `true` when a correction voided this op. It stays in the list so an
    /// app can render history, and the SDK's `fold` skips it. An app that
    /// walks `ops` itself instead of folding will double-count these — which
    /// is why the SDK ships the fold.
    pub voided: bool,
    /// The app's act. `None` is a correction that only voids, and states no
    /// replacement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Admission {
    /// Every admitted op in the total order `(ts_unix, actor, id)`, voided
    /// ones included and marked. An app folds this; it never sorts it.
    pub ops: Vec<AdmittedOp>,
    /// Everything the rail could not account for, sorted and deduplicated.
    pub gaps: Vec<RailGap>,
    /// How many journal lines this node holds, including the ones that did
    /// not survive admission. `held - ops.len()` is what was refused.
    pub held: usize,
}

impl Admission {
    /// Whether this answer covers everything we know about. A `false` here is
    /// the signal a UI must not hide: the acts are real but they are a subset.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
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
    skipped: &[oplog::SkippedLine],
    roster: &Roster,
    namespace: &str,
    verifier: &dyn RingVerifier,
) -> Admission {
    use oplog::SkippedLine;

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

    // ── signature, then membership ───────────────────────────
    let mut admitted: BTreeMap<OpId, Candidate<'_>> = BTreeMap::new();
    for op in ops {
        let derived = derived_id(op);
        if derived != op.id {
            gaps.push(RailGap::TamperedId {
                claimed: op.id.clone(),
                derived: derived.clone(),
            });
        }
        let body = body_json(&op.kind.act);
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
        let Some(person) = roster.person_for(&op.actor) else {
            gaps.push(RailGap::UnknownSigner {
                id: derived,
                actor: op.actor.clone(),
            });
            continue;
        };
        // Dedupe: the same op reaching us twice is the normal case under
        // gossip, not an anomaly.
        admitted.insert(
            derived.clone(),
            Candidate {
                id: derived,
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
        let highest = by_seq.keys().copied().next_back().unwrap_or(0);
        for n in 0..=highest {
            if !by_seq.contains_key(&n) {
                gaps.push(RailGap::SequenceHole {
                    actor: (*actor).to_string(),
                    missing: n,
                });
            }
        }
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
    // Built from every surviving correction at once, with no regard for
    // order and no regard for whether the correction is itself corrected.
    // Both choices are the same choice: the set must not depend on a walk.
    // Correcting a correction therefore cancels ITS replacement and leaves
    // the original voided — to bring something back, write it again. That is
    // what "compensating entry, visible" means, and it is why this is one
    // scan rather than a liveness pass.
    let mut voided: BTreeSet<OpId> = BTreeSet::new();
    for a in admitted.values() {
        if let RailAct::Correct { corrects, .. } = &a.op.kind.act {
            if !admitted.contains_key(corrects) {
                gaps.push(RailGap::DanglingCorrection {
                    by: a.id.clone(),
                    missing: corrects.clone(),
                });
            }
            voided.insert(corrects.clone());
        }
    }

    // ── the content-derived total order ──────────────────────
    let mut order: Vec<&Candidate<'_>> = admitted.values().collect();
    order.sort_by(|x, y| {
        (x.op.ts_unix, &x.op.actor, &x.id).cmp(&(y.op.ts_unix, &y.op.actor, &y.id))
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
    }
}

/// Re-derive the op's id from its content, ignoring the one on the line.
///
/// Identity from essence (ARCH §7.5). A rewritten `id` field therefore cannot
/// make an op impersonate another op's correction target; it just gets
/// reported.
fn derived_id(op: &Op<SignedOp>) -> OpId {
    Op::new(op.kind.clone(), op.ts_unix, op.actor.clone()).id
}

/// The exact bytes the signature covers — the act alone, in declaration
/// order, with its payload canonical (see [`Payload`](crate::Payload)).
pub fn body_json(act: &RailAct) -> String {
    serde_json::to_string(act).unwrap_or_default()
}
