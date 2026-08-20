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
use crate::oplog::{Journaled, Op};

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

impl Op<ReconciliationAct> {
    pub fn merge(
        inputs: Vec<AtomId>,
        output: AtomId,
        signals: Vec<MergeSignal>,
        judge: Option<JudgeTrace>,
        rationale: impl Into<String>,
    ) -> Self {
        Op::now(
            ReconciliationAct {
                op: OpKind::Merge,
                inputs,
                output: Some(output),
                split_outputs: Vec::new(),
                signals,
                judge_outcome: judge,
                rationale: rationale.into(),
            },
            RECONCILER,
        )
    }

    pub fn split(input: AtomId, outputs: Vec<AtomId>, rationale: impl Into<String>) -> Self {
        Op::now(
            ReconciliationAct {
                op: OpKind::Split,
                inputs: vec![input],
                output: None,
                split_outputs: outputs,
                signals: vec![MergeSignal::Other("operator".into())],
                judge_outcome: None,
                rationale: rationale.into(),
            },
            // A split is operator-driven; the signal set already says so.
            "human:operator",
        )
    }
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
