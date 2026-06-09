// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enrichment checkpoint — resume state for the field model pipeline.
//!
//! Written to `_enrichment_checkpoint.json` in the index directory.
//! Each phase sets its completion flag before moving to the next.
//! Cleared on successful completion of all phases.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Persisted enrichment progress checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnrichmentCheckpoint {
    pub schema_version: u32,
    pub corpus_id: String,
    pub domain_id: String,
    pub prompt_version: String,
    pub started_at: String, // RFC3339
    pub last_updated: String,

    pub phase_1_complete: bool,
    /// Number of skeleton extraction batches completed so far.
    /// Saved every 50 batches alongside the partial skeleton flush.
    /// Used to skip already-processed batches on resume.
    #[serde(default)]
    pub phase_1_batches_done: usize,
    /// Phase 1b — entity extraction. Opt-in per domain via
    /// `Domain::entity_extraction_prompt`. Defaults to `false` on
    /// existing checkpoints (serde default), so older runs replay the
    /// step on resume rather than silently skipping it.
    #[serde(default)]
    pub phase_1b_complete: bool,
    /// Number of entity-extraction batches whose parsed responses
    /// have been persisted to `_phase_1b_parsed.jsonl`. Updated
    /// after every successful parse so a process killed mid-phase
    /// resumes from where it left off rather than re-inferring.
    /// The JSONL is the truth source on resume; this counter is
    /// kept in sync for human-readable progress display.
    #[serde(default)]
    pub phase_1b_batches_done: usize,
    pub phase_2_complete: bool,  // clustering done
    pub phase_2b_complete: bool, // labeling done
    pub phase_3_complete: bool,  // alignment done
    pub phase_4_complete: bool,  // fault lines done
    pub phase_5_complete: bool,  // open questions done

    /// Set when the run was intentionally interrupted.
    /// Cleared on successful completion.
    pub interrupted: bool,
}

impl EnrichmentCheckpoint {
    pub fn path(index_dir: &Path) -> PathBuf {
        index_dir.join("_enrichment_checkpoint.json")
    }

    pub fn load(index_dir: &Path) -> Result<Option<Self>, Error> {
        let path = Self::path(index_dir);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        let cp: Self =
            serde_json::from_str(&raw).map_err(|e| Error::Serialization(e.to_string()))?;
        Ok(Some(cp))
    }

    pub fn save(&self, index_dir: &Path) -> Result<(), Error> {
        let path = Self::path(index_dir);
        let json =
            serde_json::to_string_pretty(self).map_err(|e| Error::Serialization(e.to_string()))?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn clear(index_dir: &Path) -> Result<(), Error> {
        let path = Self::path(index_dir);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn next_phase(&self) -> EnrichmentPhase {
        if !self.phase_1_complete {
            EnrichmentPhase::SkeletonExtraction
        } else if !self.phase_1b_complete {
            EnrichmentPhase::EntityExtraction
        } else if !self.phase_2_complete {
            EnrichmentPhase::Clustering
        } else if !self.phase_2b_complete {
            EnrichmentPhase::ClusterLabeling
        } else if !self.phase_3_complete {
            EnrichmentPhase::Alignment
        } else if !self.phase_4_complete {
            EnrichmentPhase::FaultLineDetection
        } else if !self.phase_5_complete {
            EnrichmentPhase::OpenQuestions
        } else {
            EnrichmentPhase::Complete
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnrichmentPhase {
    SkeletonExtraction,
    /// Phase 1b — Person / Organization / Initiative atom extraction
    /// for the personal + conversational domains. Opt-in per domain.
    /// Domains that don't override `entity_extraction_prompt` skip
    /// straight to Clustering.
    EntityExtraction,
    Clustering,
    ClusterLabeling,
    Alignment,
    FaultLineDetection,
    OpenQuestions,
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_phase_progression() {
        let mut cp = EnrichmentCheckpoint::default();
        assert_eq!(cp.next_phase(), EnrichmentPhase::SkeletonExtraction);

        cp.phase_1_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::EntityExtraction);

        cp.phase_1b_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::Clustering);

        cp.phase_2_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::ClusterLabeling);

        cp.phase_2b_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::Alignment);

        cp.phase_3_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::FaultLineDetection);

        cp.phase_4_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::OpenQuestions);

        cp.phase_5_complete = true;
        assert_eq!(cp.next_phase(), EnrichmentPhase::Complete);
    }

    #[test]
    fn save_load_round_trip() {
        let dir = std::env::temp_dir().join("checkpoint_test");
        std::fs::create_dir_all(&dir).unwrap();

        let cp = EnrichmentCheckpoint {
            schema_version: 1,
            corpus_id: "test".into(),
            domain_id: "philosophy".into(),
            prompt_version: "1.0.0".into(),
            started_at: "2026-04-09T00:00:00Z".into(),
            last_updated: "2026-04-09T00:00:00Z".into(),
            phase_1_complete: true,
            phase_1b_complete: true,
            phase_2_complete: true,
            ..Default::default()
        };
        cp.save(&dir).unwrap();

        let loaded = EnrichmentCheckpoint::load(&dir).unwrap().unwrap();
        assert_eq!(loaded.corpus_id, "test");
        assert!(loaded.phase_1_complete);
        assert!(loaded.phase_1b_complete);
        assert!(loaded.phase_2_complete);
        assert!(!loaded.phase_2b_complete);
        assert_eq!(loaded.next_phase(), EnrichmentPhase::ClusterLabeling);

        EnrichmentCheckpoint::clear(&dir).unwrap();
        assert!(EnrichmentCheckpoint::load(&dir).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
