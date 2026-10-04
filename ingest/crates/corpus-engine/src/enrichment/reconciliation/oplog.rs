// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reconciliation tenancy of the shared journal ([`crate::oplog`]).
//!
//! Lives at `<corpus_index>/atlas/reconciliation_oplog.jsonl`. Every merge or
//! split writes one line; the file is the bytes-level record of every
//! reconciliation decision, and reading it back replays how the current atom
//! graph reached its shape.
//!
//! The act deliberately includes the `signal` set + a `judge_outcome` field so
//! the audit trail captures exactly which signals fired and (where applicable)
//! the judge's anchor.
//!
//! # What changed on 2026-08-20
//!
//! This module used to carry its own `OplogWriter`, its own `OplogReader` and
//! its own envelope — the same twenty lines of file IO as
//! `enrichment::governance` and `meta_atlas::bridge`, which said so in their
//! own comments. All three now share [`crate::oplog`], and this log inherits
//! the three things it never had: a content-addressed [`crate::oplog::OpId`],
//! an `actor`, and a line-format version gate.
//!
//! The id in particular was not cosmetic. [`OpKind::Split`] documented itself
//! as reversible by "walking backwards finding the matching `Merge`", which is
//! unimplementable without an id to match on; governance already had the
//! answer in `GovernanceOpKind::Revert { targets: Vec<OpId> }`.

use serde::{Deserialize, Serialize};

use super::signals::MergeSignal;
use crate::enrichment::atlas::atoms::AtomId;
use crate::error::{Error, Result};
use corpus_engine_yield::time::unix_now;

use crate::oplog::{Journaled, Op, OpId};

/// Kind of op the entry records. `Split` is included from day one
/// (the architecture-over-Enron Phase 4 primitive is **reversible**;
/// the pre-Phase-4 `entity_extraction::merge_responses` merger is not
/// and stays in place for the LLM-batch dedupe path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Merge,
    Split,
}

/// Outcome of a judge trial, when the policy escalated to the judge.
/// `anchor` matches the 0..=3 scale from
/// `sovereign-agent-bench::judge_multi::MultiTrialOutcome`. `None`
/// when no judge was involved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeTrace {
    pub anchor: u8,
    /// `mean(anchor_i / 3.0)` across trials, matching the eval bank's
    /// coverage signal.
    pub coverage_mean: f64,
    pub trial_count: u8,
}

/// What a reconciliation line records — the act, carried inside a
/// [`crate::oplog::Op`] envelope that supplies the id, the timestamp, the
/// actor and the format version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationAct {
    pub op: OpKind,
    /// Atom ids feeding the op. For `Merge`, every input collapses
    /// into `output`. For `Split`, the single input becomes the
    /// outputs in `split_outputs`.
    pub inputs: Vec<AtomId>,
    /// For `Merge`, the canonical id the inputs collapsed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<AtomId>,
    /// For `Split`, the two-or-more atoms the input dissolved into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split_outputs: Vec<AtomId>,
    /// Signals that fired in support of the op. Mostly populated on
    /// `Merge`; on operator-driven `Split` the signal set is
    /// `[MergeSignal::Other("operator")]`.
    pub signals: Vec<MergeSignal>,
    /// Judge trace, when the policy escalated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_outcome: Option<JudgeTrace>,
    /// Free-text rationale; reader-facing one-liner the operator
    /// sees in the audit panel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
    /// The op this one REVERSES, when it is an undo rather than a fresh
    /// operator judgement. `None` on an operator-driven split.
    ///
    /// This is the field the module doc above says was missing: a `Split`
    /// that only records its outputs is indistinguishable from an operator
    /// deciding, today, that two atoms were never the same thing. A replay
    /// cannot tell those apart, and the difference decides whether the
    /// forward merge should be re-applied. Governance already answered this
    /// with `GovernanceOpKind::Revert { targets: Vec<OpId> }`; this is the
    /// same answer, in the same shape, for the same reason.
    ///
    /// `serde(default)` so every line written before this field existed still
    /// parses — as `None`, which is the honest reading: they record no
    /// reversal because none was expressible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<OpId>,
}

/// Who a machine-authored reconciliation line is attributed to. The
/// reconciliation primitive has no human in the loop — an honest machine label
/// beats an empty `actor`, and governance's `first_unattended_act` guard shows
/// what a log that DOES require a human does about it.
pub const RECONCILER: &str = "reconcile:multi-origin";

impl Journaled for ReconciliationAct {
    const FILE: &'static str = "reconciliation_oplog.jsonl";
    const ID_PREFIX: &'static str = "recon";
    const LABEL: &'static str = "reconciliation_oplog";
}

/// The reconciler's two acts, hung on the envelope that carries them.
///
/// Extension trait, not an inherent `impl`: [`Op`] is foreign as of 2026-09-04
/// (see `oplog`'s crate docs). A merge is a statement about atoms, which the
/// journal has never heard of.
pub trait ReconciliationOp: Sized {
    fn merge(
        inputs: Vec<AtomId>,
        output: AtomId,
        signals: Vec<MergeSignal>,
        judge: Option<JudgeTrace>,
        rationale: impl Into<String>,
    ) -> Self;
    fn split(input: AtomId, outputs: Vec<AtomId>, rationale: impl Into<String>) -> Self;
}

impl ReconciliationOp for Op<ReconciliationAct> {
    fn merge(
        inputs: Vec<AtomId>,
        output: AtomId,
        signals: Vec<MergeSignal>,
        judge: Option<JudgeTrace>,
        rationale: impl Into<String>,
    ) -> Self {
        Op::new(
            ReconciliationAct {
                op: OpKind::Merge,
                inputs,
                output: Some(output),
                split_outputs: Vec::new(),
                signals,
                judge_outcome: judge,
                rationale: rationale.into(),
                reverts: None,
            },
            unix_now(),
            RECONCILER,
        )
    }

    fn split(input: AtomId, outputs: Vec<AtomId>, rationale: impl Into<String>) -> Self {
        Op::new(
            ReconciliationAct {
                op: OpKind::Split,
                inputs: vec![input],
                output: None,
                split_outputs: outputs,
                signals: vec![MergeSignal::Other("operator".into())],
                judge_outcome: None,
                rationale: rationale.into(),
                reverts: None,
            },
            unix_now(),
            // A split is operator-driven; the signal set already says so.
            "human:operator",
        )
    }
}

/// Reverse a recorded merge, reading its inputs BACK OFF THE LOG.
///
/// This is what makes `OpKind::Split` a reversal rather than a second,
/// independent decision. [`Op::split`] takes an operator-supplied `into`
/// list, so nothing checks that the atoms coming out are the atoms that went
/// in — and its only caller today is its own test. Undo that has to be told
/// what to restore is not undo.
///
/// The envelope already carried everything needed: [`Op::id`] is
/// content-addressed and stable across replays, and the act records the
/// merge's `inputs` and `output`. Matching an id against the log is exactly
/// what `governance::derive_active` does with
/// `GovernanceOpKind::Revert { targets }`; this is the same lookup for the
/// reconciliation tenancy.
///
/// Pure over a slice rather than reading the file itself, so a caller that
/// already holds the log does not read it twice and the decision can be
/// exercised without one.
///
/// Refuses rather than guessing (ARCH §18.3):
/// - an id no line carries — the log does not describe this merge;
/// - an id that names a `Split` — reversing a reversal is a re-merge, and
///   silently emitting a Split for it would DOUBLE the undo;
/// - a `Merge` with no recorded `output` or no `inputs` — a malformed line
///   cannot say what to restore, and an empty split is not a reversal.
pub fn reverse_merge(
    ops: &[Op<ReconciliationAct>],
    merge_id: &OpId,
    rationale: impl Into<String>,
) -> Result<Op<ReconciliationAct>> {
    let forward = ops.iter().find(|op| &op.id == merge_id).ok_or_else(|| {
        Error::InvalidInput(format!(
            "reconciliation oplog carries no op `{merge_id}` — nothing to reverse"
        ))
    })?;
    if forward.kind.op != OpKind::Merge {
        return Err(Error::InvalidInput(format!(
            "op `{merge_id}` is a {:?}, not a Merge — a split is reversed by re-merging, \
             and emitting another split would undo it twice",
            forward.kind.op
        )));
    }
    let canonical = forward.kind.output.clone().ok_or_else(|| {
        Error::InvalidInput(format!(
            "merge `{merge_id}` records no output atom — the line cannot say what to split"
        ))
    })?;
    if forward.kind.inputs.is_empty() {
        return Err(Error::InvalidInput(format!(
            "merge `{merge_id}` records no inputs — there is nothing to restore"
        )));
    }
    let mut op = Op::split(canonical, forward.kind.inputs.clone(), rationale);
    op.kind.reverts = Some(forward.id.clone());
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oplog::Oplog;

    #[test]
    fn merge_round_trips_through_writer() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let entry = Op::merge(
            vec![
                AtomId::from_raw("entity-001"),
                AtomId::from_raw("entity-002"),
            ],
            AtomId::from_raw("entity-canonical-ken-lay"),
            vec![MergeSignal::NameSimilarity, MergeSignal::EmailHeader],
            None,
            "merged Ken Lay surface forms",
        );
        log.append(&entry).unwrap();
        let read = log.read_all().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].kind.op, OpKind::Merge);
        assert_eq!(read[0].kind.inputs.len(), 2);
    }

    #[test]
    fn split_records_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let entry = Op::split(
            AtomId::from_raw("entity-fused"),
            vec![AtomId::from_raw("entity-a"), AtomId::from_raw("entity-b")],
            "operator reversed prior merge",
        );
        log.append(&entry).unwrap();
        let read = log.read_all().unwrap();
        assert_eq!(read[0].kind.op, OpKind::Split);
        assert_eq!(read[0].kind.split_outputs.len(), 2);
        assert_eq!(read[0].kind.signals.len(), 1);
        assert!(matches!(&read[0].kind.signals[0], MergeSignal::Other(s) if s == "operator"));
    }

    #[test]
    fn judge_trace_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let entry = Op::merge(
            vec![AtomId::from_raw("entity-001")],
            AtomId::from_raw("entity-output"),
            vec![MergeSignal::JudgeConfirmed],
            Some(JudgeTrace {
                anchor: 3,
                coverage_mean: 1.0,
                trial_count: 3,
            }),
            "judge confirmed",
        );
        log.append(&entry).unwrap();
        let read = log.read_all().unwrap();
        let trace = read[0].kind.judge_outcome.as_ref().expect("judge_outcome");
        assert_eq!(trace.anchor, 3);
        assert_eq!(trace.trial_count, 3);
    }

    /// covers: EN-16
    ///
    /// The reversal reads the atoms to restore OUT OF THE LOG. That is the
    /// whole difference between an undo and a second decision: `split_atom`
    /// takes an operator-supplied `into` list, so nothing checks that the
    /// atoms coming out are the atoms that went in, and a caller who
    /// mis-remembers the merge produces a "reversal" that restores something
    /// else entirely — with a perfectly clean audit line.
    #[test]
    fn a_merge_is_reversed_from_the_log_with_no_operator_supplied_output_list() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let inputs = vec![
            AtomId::from_raw("entity-ken-lay"),
            AtomId::from_raw("entity-k-lay"),
            AtomId::from_raw("entity-lay-ken"),
        ];
        let merge = Op::merge(
            inputs.clone(),
            AtomId::from_raw("entity-canonical-ken-lay"),
            vec![MergeSignal::NameSimilarity, MergeSignal::EmailHeader],
            None,
            "collapsed Ken Lay surface forms",
        );
        log.append(&merge).unwrap();
        // A second, unrelated merge — so finding the right line is a lookup
        // and not "the only op there is".
        log.append(&Op::merge(
            vec![AtomId::from_raw("entity-skilling")],
            AtomId::from_raw("entity-canonical-skilling"),
            vec![MergeSignal::NameSimilarity],
            None,
            "unrelated",
        ))
        .unwrap();

        let ops = log.read_all().unwrap();
        let undo = reverse_merge(&ops, &merge.id, "operator: these were two people")
            .expect("a recorded merge must be reversible from its id alone");

        assert_eq!(undo.kind.op, OpKind::Split);
        assert_eq!(
            undo.kind.split_outputs, inputs,
            "the split must restore exactly the atoms the merge consumed"
        );
        assert_eq!(
            undo.kind.inputs,
            vec![AtomId::from_raw("entity-canonical-ken-lay")],
            "and it must dissolve the canonical atom the merge produced"
        );
        // Stamped with what it undoes, so a replay can tell a reversal from a
        // fresh operator judgement — the governance `Revert { targets }`
        // shape, for the same reason.
        assert_eq!(undo.kind.reverts.as_ref(), Some(&merge.id));

        // It round-trips: the reversal is a line like any other, and reading
        // it back preserves the link.
        log.append(&undo).unwrap();
        let ops = log.read_all().unwrap();
        let stored = ops.last().expect("the reversal was appended");
        assert_eq!(stored.kind.reverts.as_ref(), Some(&merge.id));
        assert_eq!(stored.kind.split_outputs, inputs);

        // An operator-driven split carries no reversal link — the two are
        // distinguishable on the log, which is the point of the field.
        let fresh = Op::split(
            AtomId::from_raw("entity-other"),
            vec![AtomId::from_raw("x"), AtomId::from_raw("y")],
            "operator judgement, not an undo",
        );
        assert!(fresh.kind.reverts.is_none());
    }

    /// covers: EN-16
    ///
    /// Every way the log cannot answer is a refusal, never a plausible split
    /// (ARCH §18.3). Each of these would otherwise produce a well-formed
    /// audit line recording a reversal that reversed nothing.
    #[test]
    fn a_reversal_the_log_cannot_support_is_refused_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let merge = Op::merge(
            vec![AtomId::from_raw("a"), AtomId::from_raw("b")],
            AtomId::from_raw("canonical"),
            vec![MergeSignal::NameSimilarity],
            None,
            "collapsed",
        );
        log.append(&merge).unwrap();
        let ops = log.read_all().unwrap();

        // An id no line carries.
        let err = reverse_merge(&ops, &OpId::from_raw("recon-nope"), "r")
            .expect_err("an unknown op id must refuse");
        assert!(format!("{err}").contains("carries no op"), "{err}");

        // An id that names a Split. Reversing a reversal is a RE-MERGE;
        // emitting another Split for it would undo the merge twice.
        let undo = reverse_merge(&ops, &merge.id, "undo").unwrap();
        log.append(&undo).unwrap();
        let ops = log.read_all().unwrap();
        let err = reverse_merge(&ops, &undo.id, "r")
            .expect_err("a split is not reversed by another split");
        assert!(format!("{err}").contains("not a Merge"), "{err}");

        // A malformed Merge line: no output atom, so nothing names what to
        // dissolve. Built by hand because no constructor can produce it.
        let mut broken = merge.clone();
        broken.kind.output = None;
        let err = reverse_merge(std::slice::from_ref(&broken), &broken.id, "r")
            .expect_err("a merge with no output cannot be reversed");
        assert!(format!("{err}").contains("records no output"), "{err}");

        // And one with no inputs: an empty split is not a reversal.
        let mut broken = merge.clone();
        broken.kind.inputs = Vec::new();
        let err = reverse_merge(std::slice::from_ref(&broken), &broken.id, "r")
            .expect_err("a merge with no inputs has nothing to restore");
        assert!(format!("{err}").contains("nothing to restore"), "{err}");
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        assert!(log.read_all().unwrap().is_empty());
    }

    /// The defect the shared journal fixes: before 2026-08-20 a reconciliation
    /// line had no id, so `OpKind::Split`'s documented "walk backwards finding
    /// the matching `Merge`" had nothing to match on.
    #[test]
    fn every_line_is_addressable_and_attributed() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<ReconciliationAct> = Oplog::new(dir.path());
        let merge = Op::merge(
            vec![AtomId::from_raw("a"), AtomId::from_raw("b")],
            AtomId::from_raw("canonical"),
            vec![MergeSignal::NameSimilarity],
            None,
            "collapsed",
        );
        log.append(&merge).unwrap();
        let read = log.read_all().unwrap();
        assert!(read[0].id.as_str().starts_with("recon-"));
        assert_eq!(read[0].actor, RECONCILER);
        assert_eq!(read[0].id, merge.id, "the id must survive the round trip");
    }
}
