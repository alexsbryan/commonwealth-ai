// SPDX-License-Identifier: AGPL-3.0-or-later
//! Investigation Phase-1 checkpoint — per-chunk, append-only resume.
//!
//! Mirrors the atlas pipeline's `_checkpoint.jsonl` (see
//! [`crate::enrichment::pipeline::runner`]): one JSONL row per chunk,
//! appended the instant a chunk's extraction settles — parsed successfully or
//! permanently skipped after exhausting retries. A daemon/host crash mid-run
//! therefore never discards completed extractions: a re-run reads the file,
//! skips every chunk already recorded, and resumes from the tail.
//!
//! The investigation pass is a multi-hour 35B run over hundreds of chunks
//! (the UAP hero set is ~710); without this, a single fatal fault throws away
//! all prior work. With it, the run is restartable and idempotent — the
//! canonical `entities.json` / `relationships.json` / `findings.json` outputs
//! are the durable result, and the checkpoint is deleted once they're written.
//!
//! Crash-atomicity: each row is one `writeln!` (`\n`-terminated). A crash
//! mid-write at most truncates the in-flight line; [`read_checkpoint`] skips
//! empty lines and aborts loudly on a malformed one rather than producing a
//! quietly-incomplete resume.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::extract::ExtractedChunk;
use crate::error::{Error, Result};

/// Filename of the per-chunk Phase-1 checkpoint, written inside the
/// `investigation/` output directory next to the canonical graph JSON.
pub const PHASE1_CHECKPOINT_FILENAME: &str = "_phase1_checkpoint.jsonl";

/// One JSONL row. `Success` carries the full [`ExtractedChunk`] so the entity
/// map + relationships can be rebuilt from the checkpoint alone on resume —
/// no LLM re-call. `Skipped` records a chunk whose extraction permanently
/// failed (retries exhausted) so a resume doesn't retry it forever; the run
/// already decided to drop it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChunkCheckpointEntry {
    Success {
        chunk_id: String,
        extracted: ExtractedChunk,
    },
    Skipped {
        chunk_id: String,
        reason: String,
    },
}

impl ChunkCheckpointEntry {
    pub fn chunk_id(&self) -> &str {
        match self {
            Self::Success { chunk_id, .. } => chunk_id,
            Self::Skipped { chunk_id, .. } => chunk_id,
        }
    }
}

/// Path to the checkpoint inside an investigation output directory (the
/// `investigation/` subdir under the corpus index dir).
pub fn checkpoint_path(investigation_dir: &Path) -> PathBuf {
    investigation_dir.join(PHASE1_CHECKPOINT_FILENAME)
}

/// Read every entry. A missing file returns `Ok(Vec::new())` — "nothing
/// processed yet". A malformed line aborts with an error rather than silently
/// skipping, so a corrupted checkpoint can never produce a quietly-incomplete
/// resume.
pub fn read_checkpoint(path: &Path) -> Result<Vec<ChunkCheckpointEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::Database(format!(
            "read investigation checkpoint {}: {e}",
            path.display()
        ))
    })?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: ChunkCheckpointEntry = serde_json::from_str(line).map_err(|e| {
            Error::Serialization(format!(
                "investigation checkpoint {} line {}: {e}",
                path.display(),
                i + 1
            ))
        })?;
        out.push(entry);
    }
    Ok(out)
}

/// The set of chunk ids the checkpoint considers already handled (success or
/// skipped). The resume loop skips any chunk whose id is in this set.
pub fn processed_ids(entries: &[ChunkCheckpointEntry]) -> HashSet<String> {
    entries.iter().map(|e| e.chunk_id().to_string()).collect()
}

/// Collapse the checkpoint into the deduped `(chunk_id, ExtractedChunk)`
/// successes, last-write-wins by chunk id. Feeds the in-memory accumulators on
/// resume so prior extractions are reused without re-inference. A later
/// `Skipped` for an id that previously succeeded does NOT drop the success
/// (a successful extraction is strictly better than a skip).
pub fn collapse_successes(entries: &[ChunkCheckpointEntry]) -> Vec<(String, ExtractedChunk)> {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, ExtractedChunk> = HashMap::new();
    for entry in entries {
        if let ChunkCheckpointEntry::Success {
            chunk_id,
            extracted,
        } = entry
        {
            by_id.insert(chunk_id.clone(), extracted.clone());
        }
    }
    let mut out: Vec<(String, ExtractedChunk)> = by_id.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Append one entry. Creates parent dirs + the file on first write. Atomic at
/// the line level (`writeln!` + `\n`).
pub fn append_checkpoint(path: &Path, entry: &ChunkCheckpointEntry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Database(format!(
                "create investigation checkpoint parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            Error::Database(format!(
                "open investigation checkpoint {}: {e}",
                path.display()
            ))
        })?;
    let line = serde_json::to_string(entry).map_err(|e| {
        Error::Serialization(format!("serialise investigation checkpoint entry: {e}"))
    })?;
    writeln!(f, "{line}").map_err(|e| {
        Error::Database(format!(
            "write investigation checkpoint {}: {e}",
            path.display()
        ))
    })?;
    Ok(())
}

/// Delete the checkpoint. Called once Phase 1's results have been folded into
/// the durable graph outputs, so a *fresh* re-enrich starts from zero rather
/// than skipping every chunk against a stale checkpoint. Missing file is OK.
pub fn clear_checkpoint(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| {
            Error::Database(format!(
                "remove investigation checkpoint {}: {e}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::investigation::extract::{ExtractedEntity, ExtractedRelationship};

    fn chunk_with_one_rel() -> ExtractedChunk {
        ExtractedChunk {
            entities: vec![ExtractedEntity {
                name: "Microsoft".into(),
                entity_type: "company".into(),
                attributes: Default::default(),
            }],
            relationships: vec![ExtractedRelationship {
                from_entity: "Microsoft".into(),
                to_entity: "OpenAI".into(),
                from_type: "company".into(),
                to_type: "company".into(),
                relationship_type: "investment".into(),
                attributes: Default::default(),
                verbatim_excerpt: "Microsoft invested in OpenAI".into(),
                confidence: 0.9,
            }],
        }
    }

    #[test]
    fn missing_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path(dir.path());
        assert!(read_checkpoint(&path).unwrap().is_empty());
        assert!(processed_ids(&[]).is_empty());
    }

    #[test]
    fn append_then_read_round_trips_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path(dir.path());

        append_checkpoint(
            &path,
            &ChunkCheckpointEntry::Success {
                chunk_id: "chunk-0".into(),
                extracted: chunk_with_one_rel(),
            },
        )
        .unwrap();
        append_checkpoint(
            &path,
            &ChunkCheckpointEntry::Skipped {
                chunk_id: "chunk-1".into(),
                reason: "retries exhausted".into(),
            },
        )
        .unwrap();

        let entries = read_checkpoint(&path).unwrap();
        assert_eq!(entries.len(), 2);

        // Both ids count as processed (resume skips both).
        let ids = processed_ids(&entries);
        assert!(ids.contains("chunk-0"));
        assert!(ids.contains("chunk-1"));

        // Only the success is rebuilt into the accumulators.
        let successes = collapse_successes(&entries);
        assert_eq!(successes.len(), 1);
        assert_eq!(successes[0].0, "chunk-0");
        assert_eq!(successes[0].1.relationships.len(), 1);
    }

    #[test]
    fn collapse_is_last_write_wins_and_keeps_success_over_skip() {
        let entries = vec![
            ChunkCheckpointEntry::Success {
                chunk_id: "c".into(),
                extracted: ExtractedChunk::default(),
            },
            ChunkCheckpointEntry::Success {
                chunk_id: "c".into(),
                extracted: chunk_with_one_rel(),
            },
            // A later skip for the same id must NOT erase the success.
            ChunkCheckpointEntry::Skipped {
                chunk_id: "c".into(),
                reason: "noise".into(),
            },
        ];
        let successes = collapse_successes(&entries);
        assert_eq!(successes.len(), 1);
        assert_eq!(successes[0].1.relationships.len(), 1);
    }

    #[test]
    fn malformed_line_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path(dir.path());
        std::fs::write(&path, "{not valid json}\n").unwrap();
        assert!(read_checkpoint(&path).is_err());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkpoint_path(dir.path());
        append_checkpoint(
            &path,
            &ChunkCheckpointEntry::Skipped {
                chunk_id: "x".into(),
                reason: "y".into(),
            },
        )
        .unwrap();
        assert!(path.exists());
        clear_checkpoint(&path).unwrap();
        assert!(!path.exists());
        // idempotent: clearing a missing file is fine.
        clear_checkpoint(&path).unwrap();
    }
}
