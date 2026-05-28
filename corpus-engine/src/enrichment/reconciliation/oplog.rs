//! Append-only audit log for the reconciliation primitive (Phase 4).
//!
//! Lives at `<corpus_index>/atlas/reconciliation_oplog.jsonl`. Every
//! merge or split writes one line; the file is the bytes-level record
//! of every reconciliation decision. Combined with the [`OplogReader`]
//! the operator can replay the merge sequence on a corpus to see how
//! the current atom graph reached its shape.
//!
//! The schema deliberately includes the `signal` set + an
//! `op_judge_outcome` field so the audit trail captures exactly which
//! signals fired and (where applicable) the judge's anchor. Phase 5
//! reads this file to produce the merge-signal histogram in
//! `sovereign atlas inspect --reconciliation-stats`.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::signals::MergeSignal;
use crate::enrichment::atlas::atoms::AtomId;
use crate::error::{Error, Result};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeTrace {
    pub anchor: u8,
    /// `mean(anchor_i / 3.0)` across trials, matching the eval bank's
    /// coverage signal.
    pub coverage_mean: f64,
    pub trial_count: u8,
}

/// One line in `reconciliation_oplog.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OplogEntry {
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
    /// Op timestamp (Unix seconds).
    pub ts_unix: i64,
    /// Free-text rationale; reader-facing one-liner the operator
    /// sees in the audit panel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
}

/// Append-only writer. Builds the path lazily so callers can
/// construct the writer for a corpus even if its atlas dir doesn't
/// exist yet (the first append creates it).
pub struct OplogWriter {
    pub path: PathBuf,
}

impl OplogWriter {
    pub fn new(atlas_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: atlas_dir.into().join("reconciliation_oplog.jsonl"),
        }
    }

    pub fn append(&self, entry: &OplogEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let line = serde_json::to_string(entry).map_err(|e| {
            Error::Extraction(format!("reconciliation_oplog: serialise: {e}"))
        })?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(Error::Io)?;
        f.write_all(line.as_bytes()).map_err(Error::Io)?;
        f.write_all(b"\n").map_err(Error::Io)?;
        tracing::debug!(
            op = ?entry.op,
            inputs = entry.inputs.len(),
            signals = ?entry.signals.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            judged = entry.judge_outcome.is_some(),
            "reconciliation_oplog: append"
        );
        Ok(())
    }
}

/// Streaming reader. Returns every entry in append order; the
/// recoverable scheme for `split_atom` is to walk backwards finding
/// the matching `Merge` op and reapply with the requested split.
pub struct OplogReader {
    pub path: PathBuf,
}

impl OplogReader {
    pub fn new(atlas_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: atlas_dir.into().join("reconciliation_oplog.jsonl"),
        }
    }

    pub fn read_all(&self) -> Result<Vec<OplogEntry>> {
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
            match serde_json::from_str::<OplogEntry>(&line) {
                Ok(e) => out.push(e),
                Err(err) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line = lineno + 1,
                        "reconciliation_oplog: malformed line skipped ({err})"
                    );
                }
            }
        }
        Ok(out)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl OplogEntry {
    pub fn merge(
        inputs: Vec<AtomId>,
        output: AtomId,
        signals: Vec<MergeSignal>,
        judge: Option<JudgeTrace>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            op: OpKind::Merge,
            inputs,
            output: Some(output),
            split_outputs: Vec::new(),
            signals,
            judge_outcome: judge,
            ts_unix: now_secs(),
            rationale: rationale.into(),
        }
    }

    pub fn split(
        input: AtomId,
        outputs: Vec<AtomId>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            op: OpKind::Split,
            inputs: vec![input],
            output: None,
            split_outputs: outputs,
            signals: vec![MergeSignal::Other("operator".into())],
            judge_outcome: None,
            ts_unix: now_secs(),
            rationale: rationale.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_round_trips_through_writer() {
        let dir = tempfile::tempdir().unwrap();
        let writer = OplogWriter::new(dir.path());
        let entry = OplogEntry::merge(
            vec![AtomId::from_raw("entity-001"), AtomId::from_raw("entity-002")],
            AtomId::from_raw("entity-canonical-ken-lay"),
            vec![MergeSignal::NameSimilarity, MergeSignal::EmailHeader],
            None,
            "merged Ken Lay surface forms",
        );
        writer.append(&entry).unwrap();
        let reader = OplogReader::new(dir.path());
        let read = reader.read_all().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].op, OpKind::Merge);
        assert_eq!(read[0].inputs.len(), 2);
    }

    #[test]
    fn split_records_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let writer = OplogWriter::new(dir.path());
        let entry = OplogEntry::split(
            AtomId::from_raw("entity-fused"),
            vec![AtomId::from_raw("entity-a"), AtomId::from_raw("entity-b")],
            "operator reversed prior merge",
        );
        writer.append(&entry).unwrap();
        let read = OplogReader::new(dir.path()).read_all().unwrap();
        assert_eq!(read[0].op, OpKind::Split);
        assert_eq!(read[0].split_outputs.len(), 2);
        assert_eq!(read[0].signals.len(), 1);
        assert!(matches!(&read[0].signals[0], MergeSignal::Other(s) if s == "operator"));
    }

    #[test]
    fn judge_trace_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let writer = OplogWriter::new(dir.path());
        let entry = OplogEntry::merge(
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
        writer.append(&entry).unwrap();
        let read = OplogReader::new(dir.path()).read_all().unwrap();
        let trace = read[0].judge_outcome.as_ref().expect("judge_outcome");
        assert_eq!(trace.anchor, 3);
        assert_eq!(trace.trial_count, 3);
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let read = OplogReader::new(dir.path()).read_all().unwrap();
        assert!(read.is_empty());
    }
}
