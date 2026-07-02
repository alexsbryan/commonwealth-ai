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
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::enrichment::atlas::atoms::AtomId;
use crate::enrichment::atlas::edges::EdgeId;
use crate::error::{Error, Result};

/// Current oplog line format version. Bumped only when the reader must
/// opt in to new semantics (the schema-back-compat convention: a reader
/// refuses lines declaring a `v` it doesn't understand rather than
/// silently misinterpreting them).
pub const GOVERNANCE_OPLOG_VERSION: u32 = 1;

// ── Op identity ──────────────────────────────────────────────

/// Stable, content-addressed id for a governance op. Derived from the
/// op's (kind, timestamp, actor) triple, so the id a [`GovernanceOpKind::Revert`]
/// targets is reproducible from the log bytes alone — no positional or
/// external counter to drift.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpId(String);

impl OpId {
    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── The acts ─────────────────────────────────────────────────

/// The five governance acts (spec §6.3) plus the one general reversal op.
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
    ResolveTension {
        tension: EdgeId,
        via: OpId,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        rationale: String,
    },
    /// A surfaced tension is adjudicated as known-and-tolerated. The
    /// rationale is required: an accepted contradiction must say why.
    AcceptTension { tension: EdgeId, rationale: String },
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

/// One line in `governance_oplog.jsonl` — an act plus its provenance.
///
/// The `kind` is flattened, so a line reads
/// `{"id":"gov-…","v":1,"ts_unix":…,"actor":"human:alex","op":"supersede",…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceOp {
    /// Content-addressed op id (see [`OpId`]). Stable across replays.
    pub id: OpId,
    /// Line format version. Always written; read-side gate skips lines
    /// declaring a higher version than this reader understands.
    #[serde(default = "default_version")]
    pub v: u32,
    /// Op timestamp (Unix seconds).
    pub ts_unix: i64,
    /// Who performed the act. INV-2: every act except [`GovernanceOpKind::AssertRule`]
    /// (which ingest may author as `actor = "ingest"`) MUST be attributed
    /// to a `human:<name>`. The CLI verbs pass this; [`first_unattended_act`]
    /// is the guard against an unattended write.
    pub actor: String,
    #[serde(flatten)]
    pub kind: GovernanceOpKind,
}

fn default_version() -> u32 {
    GOVERNANCE_OPLOG_VERSION
}

impl GovernanceOp {
    /// Build an op, deriving its content-addressed [`OpId`] from the
    /// (kind, ts, actor) triple. Two byte-identical acts at the same
    /// second by the same actor would collide by design — callers append
    /// in real time, so this does not arise in practice (the same
    /// birthday-bound caveat the atom content-hash ids carry).
    pub fn new(kind: GovernanceOpKind, ts_unix: i64, actor: impl Into<String>) -> Self {
        let actor = actor.into();
        // serde_json field order is the declaration order, so the body
        // string — and therefore the id — is deterministic across runs.
        let body = serde_json::to_string(&kind).unwrap_or_default();
        let input = format!("gov|{ts_unix}|{actor}|{body}");
        Self {
            id: OpId(format!("gov-{}", short_hash(&input))),
            v: GOVERNANCE_OPLOG_VERSION,
            ts_unix,
            actor,
            kind,
        }
    }
}

/// INV-2 guard: the first op that is neither an `AssertRule` nor authored
/// by a `human:<name>` actor — i.e. an adjudication a code path tried to
/// write unattended. `None` means the log honours the attribution
/// invariant. The CLI's human verbs always pass `human:<name>`; this
/// catches a regression where some automated path forges a decision.
pub fn first_unattended_act(ops: &[GovernanceOp]) -> Option<&GovernanceOp> {
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
}

/// The folded current state: every rule the log has touched with its
/// status, and every tension the log has adjudicated with its outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSet {
    /// Rule id → status. Sorted (BTreeMap) for deterministic iteration.
    pub rules: BTreeMap<AtomId, RuleStatus>,
    /// Adjudicated tension id → outcome.
    pub tensions: BTreeMap<EdgeId, TensionStatus>,
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
pub fn derive_active(ops: &[GovernanceOp]) -> ActiveSet {
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
            GovernanceOpKind::ResolveTension { tension, .. } => {
                set.tensions.insert(
                    tension.clone(),
                    TensionStatus::Resolved { by: op.id.clone() },
                );
            }
            GovernanceOpKind::AcceptTension { tension, .. } => {
                set.tensions.insert(
                    tension.clone(),
                    TensionStatus::Accepted { by: op.id.clone() },
                );
            }
            // Revert has no forward effect; its work was done in pass 1.
            GovernanceOpKind::Revert { .. } => {}
        }
    }
    set
}

// ── Persistence (mirrors reconciliation/oplog.rs conventions) ─

/// Append-only JSONL store at `<atlas_dir>/governance_oplog.jsonl`.
/// One [`GovernanceOp`] per line; the file is the bytes-level record of
/// every governance decision, and [`GovernanceOplog::derive`] replays it.
pub struct GovernanceOplog {
    pub path: PathBuf,
}

impl GovernanceOplog {
    pub fn new(atlas_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: atlas_dir.into().join("governance_oplog.jsonl"),
        }
    }

    /// Append one op. Creates the atlas dir lazily on first write.
    pub fn append(&self, op: &GovernanceOp) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let line = serde_json::to_string(op)
            .map_err(|e| Error::Extraction(format!("governance_oplog: serialise: {e}")))?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(Error::Io)?;
        f.write_all(line.as_bytes()).map_err(Error::Io)?;
        f.write_all(b"\n").map_err(Error::Io)?;
        tracing::debug!(
            id = %op.id.as_str(),
            actor = %op.actor,
            "governance_oplog: append"
        );
        Ok(())
    }

    /// Read every op in append order. Malformed lines and lines declaring
    /// a future `v` are skipped with a warning (forward-compat: an older
    /// reader must not crash on a newer log, nor silently misread it).
    pub fn read_all(&self) -> Result<Vec<GovernanceOp>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path).map_err(Error::Io)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line.map_err(Error::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<GovernanceOp>(&line) {
                Ok(op) if op.v > GOVERNANCE_OPLOG_VERSION => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line = lineno + 1,
                        v = op.v,
                        "governance_oplog: skipping op from a newer format version"
                    );
                }
                Ok(op) => out.push(op),
                Err(err) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line = lineno + 1,
                        "governance_oplog: malformed line skipped ({err})"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Read + fold in one step: the current active set for this corpus.
    pub fn derive(&self) -> Result<ActiveSet> {
        Ok(derive_active(&self.read_all()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 16-char prefix of a blake3 hex digest (64-bit truncation). Mirrors the
/// atom-id `short_hash`; kept local so the governance module doesn't widen
/// the atoms module's private API.
fn short_hash(input: &str) -> String {
    blake3::hash(input.as_bytes()).to_hex().to_string()[..16].to_string()
}

/// Unix seconds now, for the live append path (tests pass explicit ts so
/// their op ids are deterministic).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(n: usize) -> AtomId {
        AtomId::claim(n)
    }
    fn tension(n: usize) -> EdgeId {
        EdgeId::new(n)
    }

    /// Build an op at a monotonic, explicit timestamp so ids are
    /// deterministic and distinct within a test.
    fn op(kind: GovernanceOpKind, ts: i64, actor: &str) -> GovernanceOp {
        GovernanceOp::new(kind, ts, actor)
    }

    #[test]
    fn assert_then_supersede() {
        let old = rule(1);
        let new = rule(2);
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: old.clone(),
                    source_doc: Some("charter.md".into()),
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::Supersede {
                    new_rule: new.clone(),
                    old_rules: vec![old.clone()],
                    rationale: "May 12 house meeting".into(),
                },
                1001,
                "human:alex",
            ),
        ];
        let set = derive_active(&ops);
        assert!(set.is_active(&new));
        assert!(matches!(
            set.rules.get(&old),
            Some(RuleStatus::Superseded { by_rules, .. }) if by_rules == &vec![new.clone()]
        ));
        assert_eq!(set.active_rules(), vec![&new]);
    }

    #[test]
    fn retract_rule_drops_it() {
        let r = rule(1);
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: r.clone(),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::RetractRule {
                    rule: r.clone(),
                    rationale: "no longer applies".into(),
                },
                1001,
                "human:sam",
            ),
        ];
        let set = derive_active(&ops);
        assert!(!set.is_active(&r));
        assert!(matches!(
            set.rules.get(&r),
            Some(RuleStatus::Retracted { .. })
        ));
        assert!(set.active_rules().is_empty());
    }

    #[test]
    fn accept_tension_is_first_class_and_closes_open() {
        let t = tension(1);
        let other = tension(2);
        let ops = vec![op(
            GovernanceOpKind::AcceptTension {
                tension: t.clone(),
                rationale: "quiet-hours vs private-room is intentional".into(),
            },
            1000,
            "human:alex",
        )];
        let set = derive_active(&ops);
        assert!(matches!(
            set.tensions.get(&t),
            Some(TensionStatus::Accepted { .. })
        ));
        // The unadjudicated tension is still open; the accepted one is not.
        assert_eq!(set.open_tensions(&[t.clone(), other.clone()]), vec![&other]);
    }

    #[test]
    fn resolve_tension_links_to_supersede() {
        let old = rule(1);
        let new = rule(2);
        let t = tension(1);
        let supersede = op(
            GovernanceOpKind::Supersede {
                new_rule: new.clone(),
                old_rules: vec![old.clone()],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        );
        let supersede_id = supersede.id.clone();
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: old.clone(),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            supersede,
            op(
                GovernanceOpKind::ResolveTension {
                    tension: t.clone(),
                    via: supersede_id.clone(),
                    rationale: String::new(),
                },
                1002,
                "human:alex",
            ),
        ];
        let set = derive_active(&ops);
        assert!(matches!(
            set.tensions.get(&t),
            Some(TensionStatus::Resolved { .. })
        ));
        assert!(matches!(
            set.rules.get(&old),
            Some(RuleStatus::Superseded { .. })
        ));
        assert!(set.is_active(&new));
    }

    #[test]
    fn revert_supersede_reactivates_old_rule() {
        // Realistic resolution bundle: draft the new rule (AssertRule),
        // supersede the old, resolve the tension — then a human reverts
        // the whole bundle. The old rule must come back to force and the
        // tension must reopen.
        let old = rule(1);
        let new = rule(2);
        let t = tension(1);

        let assert_old = op(
            GovernanceOpKind::AssertRule {
                rule: old.clone(),
                source_doc: None,
            },
            1000,
            "ingest",
        );
        let assert_new = op(
            GovernanceOpKind::AssertRule {
                rule: new.clone(),
                source_doc: None,
            },
            1001,
            "human:alex",
        );
        let supersede = op(
            GovernanceOpKind::Supersede {
                new_rule: new.clone(),
                old_rules: vec![old.clone()],
                rationale: String::new(),
            },
            1002,
            "human:alex",
        );
        let resolve = op(
            GovernanceOpKind::ResolveTension {
                tension: t.clone(),
                via: supersede.id.clone(),
                rationale: String::new(),
            },
            1003,
            "human:alex",
        );
        let revert = op(
            GovernanceOpKind::Revert {
                targets: vec![
                    assert_new.id.clone(),
                    supersede.id.clone(),
                    resolve.id.clone(),
                ],
                rationale: "decision reversed at next meeting".into(),
            },
            1004,
            "human:sam",
        );

        let ops = vec![assert_old, assert_new, supersede, resolve, revert];
        let set = derive_active(&ops);

        // Old rule is active again; the orphan new rule and the tension
        // adjudication are gone.
        assert!(set.is_active(&old));
        assert!(
            !set.rules.contains_key(&new),
            "reverted draft rule should not exist"
        );
        assert_eq!(set.active_rules(), vec![&old]);
        assert_eq!(set.open_tensions(&[t.clone()]), vec![&t]);
    }

    #[test]
    fn revert_of_revert_reapplies() {
        let old = rule(1);
        let new = rule(2);

        let assert_old = op(
            GovernanceOpKind::AssertRule {
                rule: old.clone(),
                source_doc: None,
            },
            1000,
            "ingest",
        );
        let supersede = op(
            GovernanceOpKind::Supersede {
                new_rule: new.clone(),
                old_rules: vec![old.clone()],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        );
        let revert1 = op(
            GovernanceOpKind::Revert {
                targets: vec![supersede.id.clone()],
                rationale: "undo".into(),
            },
            1002,
            "human:sam",
        );
        let revert2 = op(
            GovernanceOpKind::Revert {
                targets: vec![revert1.id.clone()],
                rationale: "actually, keep the supersession".into(),
            },
            1003,
            "human:alex",
        );

        // After revert1 alone: old active.
        let after_one = derive_active(&[assert_old.clone(), supersede.clone(), revert1.clone()]);
        assert!(after_one.is_active(&old));

        // After revert2 (revert of the revert): supersession restored.
        let after_two = derive_active(&[assert_old, supersede, revert1, revert2]);
        assert!(after_two.is_active(&new));
        assert!(matches!(
            after_two.rules.get(&old),
            Some(RuleStatus::Superseded { .. })
        ));
    }

    #[test]
    fn reassert_is_idempotent() {
        let r = rule(1);
        let ops = vec![
            op(
                GovernanceOpKind::AssertRule {
                    rule: r.clone(),
                    source_doc: None,
                },
                1000,
                "ingest",
            ),
            op(
                GovernanceOpKind::AssertRule {
                    rule: r.clone(),
                    source_doc: None,
                },
                1001,
                "ingest",
            ),
        ];
        let set = derive_active(&ops);
        assert_eq!(set.active_rules(), vec![&r]);
        assert_eq!(set.rules.len(), 1);
    }

    #[test]
    fn op_ids_are_deterministic_and_distinct() {
        let kind = GovernanceOpKind::AcceptTension {
            tension: tension(1),
            rationale: "ok".into(),
        };
        // Same (kind, ts, actor) → same id.
        let a = GovernanceOp::new(kind.clone(), 5, "human:alex");
        let b = GovernanceOp::new(kind.clone(), 5, "human:alex");
        assert_eq!(a.id, b.id);
        // Different ts → different id.
        let c = GovernanceOp::new(kind, 6, "human:alex");
        assert_ne!(a.id, c.id);
        // Id is a content-hash shape.
        assert!(a.id.as_str().starts_with("gov-"));
        assert_eq!(a.id.as_str().len(), "gov-".len() + 16);
    }

    #[test]
    fn unattended_adjudication_is_detected() {
        // An AssertRule by ingest is allowed.
        let assert = op(
            GovernanceOpKind::AssertRule {
                rule: rule(1),
                source_doc: None,
            },
            1000,
            "ingest",
        );
        assert!(first_unattended_act(std::slice::from_ref(&assert)).is_none());

        // A Supersede must be human-authored.
        let forged = op(
            GovernanceOpKind::Supersede {
                new_rule: rule(2),
                old_rules: vec![rule(1)],
                rationale: String::new(),
            },
            1001,
            "ingest",
        );
        assert!(first_unattended_act(&[assert.clone(), forged.clone()]).is_some());

        let proper = op(
            GovernanceOpKind::Supersede {
                new_rule: rule(2),
                old_rules: vec![rule(1)],
                rationale: String::new(),
            },
            1001,
            "human:alex",
        );
        assert!(first_unattended_act(&[assert, proper]).is_none());
    }

    #[test]
    fn replay_round_trips_through_disk_byte_for_byte() {
        // INV-1: write → read → fold reproduces the in-memory fold, and
        // the read-back ops equal what was written.
        let dir = tempfile::tempdir().unwrap();
        let log = GovernanceOplog::new(dir.path());

        let old = rule(1);
        let new = rule(2);
        let t = tension(1);
        let assert_old = op(
            GovernanceOpKind::AssertRule {
                rule: old.clone(),
                source_doc: Some("charter.md".into()),
            },
            1000,
            "ingest",
        );
        let supersede = op(
            GovernanceOpKind::Supersede {
                new_rule: new.clone(),
                old_rules: vec![old.clone()],
                rationale: "meeting".into(),
            },
            1001,
            "human:alex",
        );
        let resolve = op(
            GovernanceOpKind::ResolveTension {
                tension: t.clone(),
                via: supersede.id.clone(),
                rationale: String::new(),
            },
            1002,
            "human:alex",
        );
        let written = vec![assert_old, supersede, resolve];
        for o in &written {
            log.append(o).unwrap();
        }

        let read_back = log.read_all().unwrap();
        assert_eq!(read_back, written, "ops must survive a disk round-trip");
        assert_eq!(
            log.derive().unwrap(),
            derive_active(&written),
            "fold over disk == fold over memory"
        );
    }

    #[test]
    fn read_skips_future_version_lines() {
        // Forward-compat: a line declaring a newer `v` is skipped, not
        // misinterpreted (schema-back-compat rule).
        let dir = tempfile::tempdir().unwrap();
        let log = GovernanceOplog::new(dir.path());
        let good = op(
            GovernanceOpKind::AssertRule {
                rule: rule(1),
                source_doc: None,
            },
            1000,
            "ingest",
        );
        log.append(&good).unwrap();
        // Hand-write a structurally-valid line from a future version.
        let future = format!(
            r#"{{"id":"gov-future00000000","v":{},"ts_unix":1001,"actor":"human:x","op":"assert_rule","rule":"claim-9999"}}"#,
            GOVERNANCE_OPLOG_VERSION + 1
        );
        let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
        f.write_all(future.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();

        let read = log.read_all().unwrap();
        assert_eq!(read.len(), 1, "future-version line should be skipped");
        assert_eq!(read[0].id, good.id);
    }

    #[test]
    fn missing_log_folds_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let set = GovernanceOplog::new(dir.path()).derive().unwrap();
        assert!(set.rules.is_empty());
        assert!(set.tensions.is_empty());
    }
}
