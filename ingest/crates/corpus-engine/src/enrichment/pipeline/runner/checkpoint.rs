// SPDX-License-Identifier: AGPL-3.0-or-later
//! Phase 1's per-chapter checkpoint journal.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::types::{ExtractedQuestion, Phase1Failure};
use crate::error::{Error, Result};

// ── Checkpoint (per-chapter, append-only) ────────────────────
//
// `_checkpoint.jsonl` is JSONL: one [`Phase1CheckpointEntry`] per
// line, appended immediately after each chapter completes (success
// or failure). Crash-resilience for long-running Phase 1 runs
// (Wikipedia-scale Tier-2 takes hours; a daemon/host crash mid-run
// would otherwise lose every successful chapter).
//
// Resume reads the file, builds a set of chapter ids already
// processed, and the CLI passes a `Subset` selection of the
// remainder. After enough invocations to cover every chapter, the
// `--finalize` path reads the JSONL once more and writes a
// canonical `Phase1Output` run-file from it.

/// One JSONL row in `_checkpoint.jsonl`. `Success` carries the full
/// `ExtractedQuestion` so a subsequent `--finalize` can rebuild the
/// run-file from the checkpoint alone — no need to keep prior
/// run-files alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Phase1CheckpointEntry {
    Success {
        chapter_id: String,
        extracted: ExtractedQuestion,
    },
    Failure {
        chapter_id: String,
        failure: Phase1Failure,
    },
}

impl Phase1CheckpointEntry {
    pub fn chapter_id(&self) -> &str {
        match self {
            Self::Success { chapter_id, .. } => chapter_id,
            Self::Failure { chapter_id, .. } => chapter_id,
        }
    }
}

/// Read every entry from a Phase-1 checkpoint file. Empty / missing
/// file returns `Ok(Vec::new())` — both mean "nothing processed yet".
/// Malformed lines abort with an error rather than silently skipping
/// (a corrupted checkpoint should not produce a quietly-incomplete
/// resume).
pub fn read_phase1_checkpoint(path: &Path) -> Result<Vec<Phase1CheckpointEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Database(format!("read checkpoint {}: {e}", path.display())))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: Phase1CheckpointEntry = serde_json::from_str(line).map_err(|e| {
            Error::Serialization(format!("checkpoint {} line {}: {e}", path.display(), i + 1))
        })?;
        out.push(entry);
    }
    Ok(out)
}

/// Start a new run's journal. A `--full` run without `--resume` continues
/// nothing, so an older run's entries (another cut, or an entry schema the
/// reader now refuses) are never read as this run's by `--retry-failed`,
/// `--resume` or `--finalize`. Returns how many entries were dropped.
pub fn begin_phase1_checkpoint(path: &Path) -> Result<usize> {
    let held = match std::fs::read_to_string(path) {
        Ok(text) => text.lines().filter(|line| !line.trim().is_empty()).count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(Error::Database(format!(
                "read checkpoint {}: {e}",
                path.display()
            )))
        }
    };
    std::fs::remove_file(path)
        .map_err(|e| Error::Database(format!("drop checkpoint {}: {e}", path.display())))?;
    Ok(held)
}

/// Aggregate a checkpoint into `(extracted, failures)` deduped by
/// chapter_id with last-write-wins semantics. Used by `--finalize`
/// when reconstructing a canonical `Phase1Output`.
pub fn collapse_phase1_checkpoint(
    entries: Vec<Phase1CheckpointEntry>,
) -> (Vec<ExtractedQuestion>, Vec<Phase1Failure>) {
    let mut by_id_success: HashMap<String, ExtractedQuestion> = HashMap::new();
    let mut by_id_failure: HashMap<String, Phase1Failure> = HashMap::new();
    for entry in entries {
        match entry {
            Phase1CheckpointEntry::Success {
                chapter_id,
                extracted,
            } => {
                by_id_failure.remove(&chapter_id);
                by_id_success.insert(chapter_id, extracted);
            }
            Phase1CheckpointEntry::Failure {
                chapter_id,
                failure,
            } => {
                if !by_id_success.contains_key(&chapter_id) {
                    by_id_failure.insert(chapter_id, failure);
                }
            }
        }
    }
    let mut extracted: Vec<ExtractedQuestion> = by_id_success.into_values().collect();
    extracted.sort_by(|a, b| a.chapter_id.cmp(&b.chapter_id));
    let mut failures: Vec<Phase1Failure> = by_id_failure.into_values().collect();
    failures.sort_by(|a, b| a.chapter_id.cmp(&b.chapter_id));
    (extracted, failures)
}

/// Build the set of chapter ids the checkpoint considers processed
/// (success + failure). Resume callers feed this into the
/// `ChapterSelection` filter.
pub fn checkpoint_processed_ids(entries: &[Phase1CheckpointEntry]) -> HashSet<String> {
    entries.iter().map(|e| e.chapter_id().to_string()).collect()
}

/// Append one entry to the checkpoint file. Atomic at the line
/// level (`writeln!` + `\n`), so a crash mid-write at most truncates
/// the in-flight line — the reader skips empty lines and aborts on
/// malformed ones.
pub(super) fn append_phase1_checkpoint(path: &Path, entry: &Phase1CheckpointEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Database(format!(
                "create checkpoint parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::Database(format!("open checkpoint {}: {e}", path.display())))?;
    let line = serde_json::to_string(entry)
        .map_err(|e| Error::Serialization(format!("serialise checkpoint entry: {e}")))?;
    writeln!(f, "{line}")
        .map_err(|e| Error::Database(format!("write checkpoint {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::pipeline::types::PhaseFailureKind;

    /// world-s4, 2026-10-20: the journal still held entries written before a
    /// particular's `words` field was deleted, so `--retry-failed` refused to
    /// start after a fresh `--full` lost a window.
    #[test]
    fn a_fresh_run_reads_none_of_an_older_runs_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("_phase1_checkpoint.jsonl");
        std::fs::write(
            &path,
            "{\"kind\":\"success\",\"chapter_id\":\"sec_00001\"}\n\n",
        )
        .unwrap();
        assert!(
            read_phase1_checkpoint(&path).is_err(),
            "the older journal is unreadable"
        );

        assert_eq!(begin_phase1_checkpoint(&path).unwrap(), 1);
        let lost = Phase1Failure {
            chapter_id: "sec_00017".into(),
            reason: "no recognisable JSON object".into(),
            raw_response_head: None,
            failure_kind: PhaseFailureKind::ParseDrift,
        };
        append_phase1_checkpoint(
            &path,
            &Phase1CheckpointEntry::Failure {
                chapter_id: "sec_00017".into(),
                failure: lost,
            },
        )
        .unwrap();
        let entries = read_phase1_checkpoint(&path).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.chapter_id()).collect::<Vec<_>>(),
            ["sec_00017"]
        );

        assert_eq!(
            begin_phase1_checkpoint(&dir.path().join("none.jsonl")).unwrap(),
            0
        );
    }
}
