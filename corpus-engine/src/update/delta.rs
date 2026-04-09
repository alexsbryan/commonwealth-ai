//! Delta corpus updater — applies incremental updates to an installed index.
//!
//! ## Delete-last invariant (for `phase_updates`)
//!
//! 1. Fetch + embed new content.
//! 2. `index.insert_chunks(&new)` — both old and new versions coexist briefly.
//! 3. `index.delete_chunks_by_source_doc(doc_id)` — removes old chunks.
//! 4. `index.mark_claims_stale_for_doc(doc_id)` — re-extraction handled by
//!    `EnrichmentChecker` on the next cycle.
//!
//! ## Resumability
//!
//! `_update_progress.json` tracks which doc_ids have completed each phase.
//! `save_stored_manifest()` is called only after all three phases complete.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::engine::CorpusEngine;
use crate::error::Result;

// ─── VersionManifest ─────────────────────────────────────────────────────────

/// A version manifest downloaded from `update_manifest_url`.
///
/// `entries` maps document ID → content hash (or version token) for each
/// document in the current dataset release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionManifest {
    pub corpus_id: String,
    /// Dataset version string (e.g. a date stamp or semver).
    pub version: String,
    /// `doc_id → content_hash` for every document in this release.
    pub entries: HashMap<String, String>,
}

// ─── ManifestDiff ─────────────────────────────────────────────────────────────

/// The delta between a stored manifest and a newly fetched one.
#[derive(Debug, Clone, Default)]
pub struct ManifestDiff {
    /// Documents present in `new` but not in `old`.
    pub new_documents: Vec<String>,
    /// Documents present in both but with different hashes.
    pub updated_documents: Vec<String>,
    /// Documents present in `old` but absent from `new`.
    pub deleted_documents: Vec<String>,
}

impl ManifestDiff {
    /// Compute the diff between `old` and `new` manifests.
    pub fn compute(old: &VersionManifest, new: &VersionManifest) -> Self {
        let mut diff = Self::default();

        for (id, new_hash) in &new.entries {
            match old.entries.get(id.as_str()) {
                None => diff.new_documents.push(id.clone()),
                Some(old_hash) if old_hash != new_hash => {
                    diff.updated_documents.push(id.clone());
                }
                _ => {}
            }
        }

        for id in old.entries.keys() {
            if !new.entries.contains_key(id.as_str()) {
                diff.deleted_documents.push(id.clone());
            }
        }

        diff
    }

    /// True when there are no changes between the two manifests.
    pub fn is_empty(&self) -> bool {
        self.new_documents.is_empty()
            && self.updated_documents.is_empty()
            && self.deleted_documents.is_empty()
    }
}

// ─── UpdateProgress ──────────────────────────────────────────────────────────

/// Progress broadcast from `CorpusUpdater`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateProgress {
    pub corpus_id: String,
    pub phase: UpdatePhase,
    pub current: usize,
    pub total: usize,
}

/// The three sequential phases of an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Deletions,
    Updates,
    Additions,
}

// ─── UpdateProgressLog ───────────────────────────────────────────────────────

/// Persistent log of which doc_ids have completed each phase.
/// Serialised to `_update_progress.json` in the corpus index directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateProgressLog {
    /// Doc IDs that have been fully deleted.
    pub deleted_ids: Vec<String>,
    /// Doc IDs that have been fully updated (old removed, new inserted).
    pub updated_ids: Vec<String>,
    /// Doc IDs that have been fully added.
    pub added_ids: Vec<String>,
}

impl UpdateProgressLog {
    pub fn is_complete(
        &self,
        diff: &ManifestDiff,
    ) -> bool {
        let deletions_done =
            diff.deleted_documents.iter().all(|id| self.deleted_ids.contains(id));
        let updates_done =
            diff.updated_documents.iter().all(|id| self.updated_ids.contains(id));
        let additions_done =
            diff.new_documents.iter().all(|id| self.added_ids.contains(id));
        deletions_done && updates_done && additions_done
    }
}

// ─── CorpusUpdater ───────────────────────────────────────────────────────────

/// Applies a `ManifestDiff` to an installed corpus index.
pub struct CorpusUpdater {
    engine: CorpusEngine,
    progress_tx: Option<mpsc::Sender<UpdateProgress>>,
}

impl CorpusUpdater {
    pub fn new(engine: CorpusEngine) -> Self {
        Self { engine, progress_tx: None }
    }

    /// Attach a channel to receive real-time progress updates.
    pub fn with_progress_tx(mut self, tx: mpsc::Sender<UpdateProgress>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// Apply a full update: deletions → updates (delete-last) → additions.
    ///
    /// Resumes from `progress_log` so interrupted updates don't re-do work.
    pub async fn apply_update(
        &self,
        corpus_id: &str,
        diff: &ManifestDiff,
        new_manifest: &VersionManifest,
        fetch_content: impl Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    ) -> Result<()> {
        let mut log = self.engine.load_update_progress(corpus_id).unwrap_or_default();

        if diff.is_empty() || log.is_complete(diff) {
            self.engine.clear_update_progress(corpus_id)?;
            self.engine.save_stored_manifest(corpus_id, new_manifest)?;
            return Ok(());
        }

        // ── Phase 1: Deletions ────────────────────────────────────────────────
        self.phase_deletions(corpus_id, diff, &mut log, &fetch_content).await?;

        // ── Phase 2: Updates (delete-last) ────────────────────────────────────
        self.phase_updates(corpus_id, diff, &mut log, &fetch_content).await?;

        // ── Phase 3: Additions ────────────────────────────────────────────────
        self.phase_additions(corpus_id, diff, &mut log, &fetch_content).await?;

        // All done — persist the new manifest and clear progress.
        self.engine.clear_update_progress(corpus_id)?;
        self.engine.save_stored_manifest(corpus_id, new_manifest)?;
        Ok(())
    }

    // ── Private phase methods ─────────────────────────────────────────────────

    async fn phase_deletions(
        &self,
        corpus_id: &str,
        diff: &ManifestDiff,
        log: &mut UpdateProgressLog,
        _fetch_content: &impl Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    ) -> Result<()> {
        let total = diff.deleted_documents.len();
        let index = self.engine.open_index_for_corpus(corpus_id).await?;

        for (i, doc_id) in diff.deleted_documents.iter().enumerate() {
            if log.deleted_ids.contains(doc_id) {
                continue;
            }
            index.delete_chunks_by_source_doc(doc_id).await?;
            log.deleted_ids.push(doc_id.clone());
            self.engine.save_update_progress(corpus_id, log)?;
            self.emit(UpdateProgress {
                corpus_id: corpus_id.into(),
                phase: UpdatePhase::Deletions,
                current: i + 1,
                total,
            }).await;
        }
        Ok(())
    }

    async fn phase_updates(
        &self,
        corpus_id: &str,
        diff: &ManifestDiff,
        log: &mut UpdateProgressLog,
        fetch_content: &impl Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    ) -> Result<()> {
        let total = diff.updated_documents.len();
        let recipe = self.engine.load_recipe(corpus_id).await?;
        let index = self.engine.open_index_for_corpus(corpus_id).await?;

        for (i, doc_id) in diff.updated_documents.iter().enumerate() {
            if log.updated_ids.contains(doc_id) {
                continue;
            }
            let content = fetch_content(doc_id).await?;
            let raw_chunks = self.engine.chunk_document(&recipe, &content)?;
            let embedded = self.engine.embed_chunks(&raw_chunks).await?;

            // Delete-last: insert new → delete old.
            index.insert_chunks(&embedded).await?;
            index.delete_chunks_by_source_doc(doc_id).await?;
            index.mark_claims_stale_for_doc(doc_id).await?;

            log.updated_ids.push(doc_id.clone());
            self.engine.save_update_progress(corpus_id, log)?;
            self.emit(UpdateProgress {
                corpus_id: corpus_id.into(),
                phase: UpdatePhase::Updates,
                current: i + 1,
                total,
            }).await;
        }
        Ok(())
    }

    async fn phase_additions(
        &self,
        corpus_id: &str,
        diff: &ManifestDiff,
        log: &mut UpdateProgressLog,
        fetch_content: &impl Fn(&str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    ) -> Result<()> {
        let total = diff.new_documents.len();
        let recipe = self.engine.load_recipe(corpus_id).await?;
        let index = self.engine.open_index_for_corpus(corpus_id).await?;

        for (i, doc_id) in diff.new_documents.iter().enumerate() {
            if log.added_ids.contains(doc_id) {
                continue;
            }
            let content = fetch_content(doc_id).await?;
            let raw_chunks = self.engine.chunk_document(&recipe, &content)?;
            let embedded = self.engine.embed_chunks(&raw_chunks).await?;

            index.insert_chunks(&embedded).await?;

            log.added_ids.push(doc_id.clone());
            self.engine.save_update_progress(corpus_id, log)?;
            self.emit(UpdateProgress {
                corpus_id: corpus_id.into(),
                phase: UpdatePhase::Additions,
                current: i + 1,
                total,
            }).await;
        }
        Ok(())
    }

    async fn emit(&self, progress: UpdateProgress) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(progress).await;
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest(corpus_id: &str, version: &str, entries: &[(&str, &str)]) -> VersionManifest {
        VersionManifest {
            corpus_id: corpus_id.into(),
            version: version.into(),
            entries: entries.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn manifest_diff_compute_all_buckets() {
        let old = make_manifest(
            "sep",
            "v1",
            &[
                ("doc-a", "hash1"),
                ("doc-b", "hash2"),
                ("doc-c", "hash3"),
            ],
        );
        let new = make_manifest(
            "sep",
            "v2",
            &[
                ("doc-b", "hash2-updated"),
                ("doc-c", "hash3"),              // unchanged
                ("doc-d", "hash4"),              // new
            ],
        );
        let diff = ManifestDiff::compute(&old, &new);
        assert_eq!(diff.new_documents, vec!["doc-d"]);
        assert_eq!(diff.updated_documents, vec!["doc-b"]);
        assert_eq!(diff.deleted_documents, vec!["doc-a"]);
    }

    #[test]
    fn manifest_diff_empty_when_identical() {
        let m = make_manifest("sep", "v1", &[("doc-a", "hash1")]);
        let diff = ManifestDiff::compute(&m, &m);
        assert!(diff.is_empty());
    }

    #[test]
    fn manifest_diff_all_new() {
        let old = make_manifest("sep", "v1", &[]);
        let new = make_manifest("sep", "v2", &[("doc-a", "h1"), ("doc-b", "h2")]);
        let diff = ManifestDiff::compute(&old, &new);
        assert_eq!(diff.new_documents.len(), 2);
        assert!(diff.updated_documents.is_empty());
        assert!(diff.deleted_documents.is_empty());
    }

    #[test]
    fn manifest_diff_all_deleted() {
        let old = make_manifest("sep", "v1", &[("doc-a", "h1")]);
        let new = make_manifest("sep", "v2", &[]);
        let diff = ManifestDiff::compute(&old, &new);
        assert!(diff.new_documents.is_empty());
        assert!(diff.updated_documents.is_empty());
        assert_eq!(diff.deleted_documents.len(), 1);
    }
}
