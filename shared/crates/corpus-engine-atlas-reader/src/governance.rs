// SPDX-License-Identifier: AGPL-3.0-or-later
//! Event-sourced governance oplog + active-set fold.
//!
//! The governance layer treats a community's normative state as a
//! *query over an append-only log of governance acts*, not as stored
//! mutable state. Rules are atlas `Claim` atoms (extracted by a
//! recipe's custom-ontology atlas pipeline); tensions are atlas
//! `EdgeType::Tension` edges. This module owns the **acts on top of
//! that graph** — assert / supersede / retract a rule, resolve / accept
//! a tension — and the pure fold ([`derive_active`]) that derives the
//! *current active rule set* from the act log.
//!
//! Why event-sourced (the design conceit, "common law"):
//!   - the current law is a [`derive_active`] query, not a row you mutate;
//!   - history is preserved — "what was the guest policy in March, and
//!     why did it change?" is answerable because nothing is destroyed;
//!   - every act is reversible via a single general [`GovernanceOpKind::Revert`]
//!     that tomb-stones prior ops during the fold (INV-3) and is itself
//!     revertible (reverting a `Revert` re-applies the original ops);
//!   - [`GovernanceOpKind::AcceptTension`] makes a known, tolerated
//!     contradiction a *first-class state*, not a bug to be force-resolved —
//!     which is how real common law actually works.
//!
//! Division of labour with the atlas: the atlas graph supplies *which
//! rules exist* (Claim atoms) and *which tensions were surfaced* (Tension
//! edges). This log supplies *what was decided about them*. The fold is
//! therefore pure over the ops alone; the set of currently-**open**
//! tensions is `(atlas tension edges) − (adjudicated edges)`, computed by
//! [`ActiveSet::open_tensions`] which takes the edge universe as input.
//!
//! This is the load-bearing contract everything else writes through, so
//! the fold does no IO and never calls inference, and the invariants
//! below (replay determinism, reversibility, attribution) are pinned by
//! the module tests.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::atoms::AtomId;
use crate::edges::EdgeId;
use crate::oplog::{Journaled, Op, OpId, Oplog};
use corpus_index::error::Result;

/// Current oplog line format version. Bumped only when the reader must
/// opt in to new semantics (the schema-back-compat convention: a reader
/// refuses lines declaring a `v` it doesn't understand rather than
/// silently misinterpreting them).
pub const GOVERNANCE_OPLOG_VERSION: u32 = 1;

/// The governance tenancy of the shared journal ([`crate::oplog`]): one act
/// per line in `<atlas_dir>/governance_oplog.jsonl`, ids prefixed `gov-`.
///
/// The envelope, the content-addressed id, the append and the version gate
/// all live in [`crate::oplog`]. This module owns only the acts and the fold.
/// Before 2026-08-20 it owned a private copy of all four, as did
/// `reconciliation::oplog` and `meta_atlas::bridge`.
impl Journaled for GovernanceOpKind {
    const FILE: &'static str = "governance_oplog.jsonl";
    const ID_PREFIX: &'static str = "gov";
    const VERSION: u32 = GOVERNANCE_OPLOG_VERSION;
    const LABEL: &'static str = "governance_oplog";
}

// ── The acts ─────────────────────────────────────────────────

/// The six governance acts (spec §6.3) plus the one general reversal op.
///
/// Internally tagged on `"op"` so each line is self-describing and each
/// variant carries exactly its own fields — illegal field combinations
/// are unrepresentable (SOLID/"make illegal states unrepresentable",
/// versus the all-fields-optional shape of the reconciliation oplog).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GovernanceOpKind {
    /// A rule (Claim atom) enters the governed set. Emitted idempotently
    /// by ingest for every freshly-extracted rule; re-asserting a rule id
    /// already present is a no-op for the active set.
    AssertRule {
        rule: AtomId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_doc: Option<String>,
    },
    /// One or more old rules are replaced by a new rule. The new rule is
    /// asserted active; each old rule becomes [`RuleStatus::Superseded`].
    Supersede {
        new_rule: AtomId,
        old_rules: Vec<AtomId>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
    /// A rule is withdrawn from active law with no replacement.
    RetractRule {
        rule: AtomId,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
    /// A surfaced tension is adjudicated as resolved. `via` names the act
    /// that resolved it (a [`GovernanceOpKind::Supersede`]), so the audit
    /// trail links the tension to its fix.
    ///
    /// `endpoints` records the tension's two rule ids at adjudication
    /// time. Edge ids are re-minted sequentially on every atlas rebuild,
    /// but content-hash rule ids are stable — so the endpoint pair is the
    /// adjudication's durable identity across rebuilds (see
    /// [`ActiveSet::tension_pairs`]). Optional for back-compat with logs
    /// written before the field existed.
    ResolveTension {
        tension: EdgeId,
        via: OpId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoints: Option<(AtomId, AtomId)>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
    /// A surfaced tension is adjudicated as known-and-tolerated. The
    /// rationale is required: an accepted contradiction must say why.
    /// `endpoints` as on [`GovernanceOpKind::ResolveTension`].
    AcceptTension {
        tension: EdgeId,
        rationale: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoints: Option<(AtomId, AtomId)>,
    },
    /// A surfaced tension is adjudicated as not-a-conflict: the detector
    /// was wrong (a decoy), not a contradiction the community tolerates —
    /// that distinct state is [`GovernanceOpKind::AcceptTension`].
    /// Deliberately light-ceremony (rationale optional): dismissing
    /// detector noise is the steward's call, and it is revertible like
    /// every act. `endpoints` as on [`GovernanceOpKind::ResolveTension`].
    DismissTension {
        tension: EdgeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoints: Option<(AtomId, AtomId)>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
    /// Tomb-stone prior op(s). The fold skips each target *and its
    /// effects* as if it had never been recorded. A human adjudication is
    /// usually a small bundle (AssertRule + Supersede + ResolveTension);
    /// naming the whole bundle makes the undo atomic. Revertible: a
    /// `Revert` of a `Revert` re-applies the originals.
    Revert {
        targets: Vec<OpId>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
}

/// INV-2 guard: the first op that is neither an `AssertRule` nor authored
/// by a `human:<name>` actor — i.e. an adjudication a code path tried to
/// write unattended. `None` means the log honours the attribution
/// invariant. The CLI's human verbs always pass `human:<name>`; this
/// catches a regression where some automated path forges a decision.
pub fn first_unattended_act(ops: &[Op<GovernanceOpKind>]) -> Option<&Op<GovernanceOpKind>> {
    ops.iter().find(|op| {
        !matches!(op.kind, GovernanceOpKind::AssertRule { .. }) && !op.actor.starts_with("human:")
    })
}

// ── Derived state ────────────────────────────────────────────

/// Status of a rule in the derived active set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RuleStatus {
    /// In force.
    Active,
    /// Replaced by `by_rules`, via op `by`.
    Superseded { by: OpId, by_rules: Vec<AtomId> },
    /// Withdrawn without replacement, via op `by`.
    Retracted { by: OpId },
}

/// Adjudication outcome of a tension. Only adjudicated tensions appear in
/// the fold output — an *open* tension is one the atlas surfaced that the
/// log has not yet touched (see [`ActiveSet::open_tensions`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TensionStatus {
    /// Resolved by op `by` (which references the resolving Supersede).
    Resolved { by: OpId },
    /// Accepted as known-and-tolerated, via op `by`.
    Accepted { by: OpId },
    /// Dismissed as not-a-conflict (detector noise), via op `by`.
    Dismissed { by: OpId },
}

impl TensionStatus {
    /// The op that produced this status. Every adjudication carries the
    /// id of the act that decided it — used to identify the *live,
    /// winning* adjudication for an edge (last-write-wins in the fold).
    pub fn by(&self) -> &OpId {
        match self {
            TensionStatus::Resolved { by }
            | TensionStatus::Accepted { by }
            | TensionStatus::Dismissed { by } => by,
        }
    }
}

/// Canonical, order-independent key for a tension's endpoint rule pair —
/// `"<lo-id>|<hi-id>"`. String-backed so [`ActiveSet`] stays
/// JSON-serializable (JSON map keys must be strings); constructed only
/// through [`PairKey::new`], which normalizes the order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PairKey(String);

impl PairKey {
    pub fn new(a: &AtomId, b: &AtomId) -> Self {
        let (lo, hi) = if a.as_str() <= b.as_str() {
            (a, b)
        } else {
            (b, a)
        };
        Self(format!("{}|{}", lo.as_str(), hi.as_str()))
    }
}

/// The folded current state: every rule the log has touched with its
/// status, and every tension the log has adjudicated with its outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSet {
    /// Rule id → status. Sorted (BTreeMap) for deterministic iteration.
    pub rules: BTreeMap<AtomId, RuleStatus>,
    /// Adjudicated tension id → outcome.
    pub tensions: BTreeMap<EdgeId, TensionStatus>,
    /// Adjudicated endpoint-pair → outcome, for adjudication ops that
    /// carried `endpoints`. Edge ids are re-minted on every atlas
    /// rebuild; the (content-hash) rule-id pair is the identity that
    /// survives, so a re-detected tension between the same two rules
    /// inherits its adjudication (see the view join in
    /// `governance_view::build_view`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tension_pairs: BTreeMap<PairKey, TensionStatus>,
}

impl ActiveSet {
    /// The rules currently in force, in deterministic id order.
    pub fn active_rules(&self) -> Vec<&AtomId> {
        self.rules
            .iter()
            .filter(|(_, s)| matches!(s, RuleStatus::Active))
            .map(|(id, _)| id)
            .collect()
    }

    /// Whether a specific rule is currently in force.
    pub fn is_active(&self, rule: &AtomId) -> bool {
        matches!(self.rules.get(rule), Some(RuleStatus::Active))
    }

    /// Open tensions = surfaced-by-the-atlas minus adjudicated-by-the-log.
    /// `all` is the tension-edge universe from the atlas graph; this
    /// returns those with no adjudication, in input order.
    pub fn open_tensions<'a>(&self, all: &'a [EdgeId]) -> Vec<&'a EdgeId> {
        all.iter()
            .filter(|e| !self.tensions.contains_key(*e))
            .collect()
    }
}

/// Derive the current active set by folding the act log.
///
/// Two passes, both pure:
///
/// **Pass 1 (backward) — liveness.** Walk newest→oldest computing which
/// ops are *live*, honouring [`GovernanceOpKind::Revert`] chains. A
/// `Revert` targets only earlier ops, so walking backward means every op
/// already knows the verdict of any op that could revert it. A live
/// `Revert` cancels its targets; a *cancelled* `Revert` does not (so
/// reverting a `Revert` re-applies the originals). See tests
/// `revert_supersede_reactivates_old_rule` and `revert_of_revert_reapplies`.
///
/// **Pass 2 (forward) — apply.** Fold the live ops in log order; later
/// ops overwrite earlier status for the same rule. Because the supersede
/// of a reverted bundle is skipped in pass 2, the old rule's earlier
/// `Active` status (from its ingest `AssertRule`) stands.
pub fn derive_active(ops: &[Op<GovernanceOpKind>]) -> ActiveSet {
    let n = ops.len();

    // Pass 1: liveness.
    let mut live = vec![true; n];
    let mut cancelled: BTreeSet<&str> = BTreeSet::new();
    for i in (0..n).rev() {
        let op = &ops[i];
        if cancelled.contains(op.id.as_str()) {
            // This op was tomb-stoned by a later live Revert. If it is
            // itself a Revert, skipping it here means its targets are
            // NOT added to `cancelled` — that is the revert-of-revert
            // re-application.
            live[i] = false;
            continue;
        }
        if let GovernanceOpKind::Revert { targets, .. } = &op.kind {
            for t in targets {
                cancelled.insert(t.as_str());
            }
        }
    }

    // Pass 2: apply live ops forward.
    let mut set = ActiveSet::default();
    for (i, op) in ops.iter().enumerate() {
        if !live[i] {
            continue;
        }
        match &op.kind {
            GovernanceOpKind::AssertRule { rule, .. } => {
                set.rules.insert(rule.clone(), RuleStatus::Active);
            }
            GovernanceOpKind::Supersede {
                new_rule,
                old_rules,
                ..
            } => {
                set.rules.insert(new_rule.clone(), RuleStatus::Active);
                for old in old_rules {
                    set.rules.insert(
                        old.clone(),
                        RuleStatus::Superseded {
                            by: op.id.clone(),
                            by_rules: vec![new_rule.clone()],
                        },
                    );
                }
            }
            GovernanceOpKind::RetractRule { rule, .. } => {
                set.rules
                    .insert(rule.clone(), RuleStatus::Retracted { by: op.id.clone() });
            }
            GovernanceOpKind::ResolveTension {
                tension, endpoints, ..
            } => {
                let status = TensionStatus::Resolved { by: op.id.clone() };
                if let Some((a, b)) = endpoints {
                    set.tension_pairs.insert(PairKey::new(a, b), status.clone());
                }
                set.tensions.insert(tension.clone(), status);
            }
            GovernanceOpKind::AcceptTension {
                tension, endpoints, ..
            } => {
                let status = TensionStatus::Accepted { by: op.id.clone() };
                if let Some((a, b)) = endpoints {
                    set.tension_pairs.insert(PairKey::new(a, b), status.clone());
                }
                set.tensions.insert(tension.clone(), status);
            }
            GovernanceOpKind::DismissTension {
                tension, endpoints, ..
            } => {
                let status = TensionStatus::Dismissed { by: op.id.clone() };
                if let Some((a, b)) = endpoints {
                    set.tension_pairs.insert(PairKey::new(a, b), status.clone());
                }
                set.tensions.insert(tension.clone(), status);
            }
            // Revert has no forward effect; its work was done in pass 1.
            GovernanceOpKind::Revert { .. } => {}
        }
    }
    set
}

// ── Persistence ──────────────────────────────────────────────
//
// The append, the read and the version gate moved to `crate::oplog` on
// 2026-08-20 — they were the same twenty lines as `reconciliation::oplog` and
// `meta_atlas::bridge`, as the section header here used to admit ("mirrors
// reconciliation/oplog.rs conventions"). What is left is the one thing only
// governance can do with its log: fold it.

/// Read + fold in one step: the current active set for this corpus. A free
/// function, not an inherent `impl`: the journal moved to the `oplog` leaf on
/// 2026-09-04, which makes [`Oplog`] foreign here.
pub fn derive(log: &Oplog<GovernanceOpKind>) -> Result<ActiveSet> {
    Ok(derive_active(&log.read_all()?))
}

#[cfg(test)]
#[path = "tests/governance.rs"]
mod tests;
