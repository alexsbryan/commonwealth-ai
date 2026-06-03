//! `try_restore_prebuilt` — extracted out of `engine::ingest`.
//!
//! Pulled into its own module so the `ingest()` orchestrator stays
//! focused on the acquire/extract/chunk/embed pipeline. Shape is
//! behaviour-preserving: same signature, same error semantics, same
//! tracing events as before the move.

use super::ingest_helpers::dir_size_recursive;
use super::CorpusEngine;
use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::progress::ProgressCallback;
use crate::recipe::Recipe;
use crate::types::IngestResult;

impl CorpusEngine {
    /// Download and extract a prebuilt-snapshot archive into the
    /// canonical index dir, bypassing the acquire/extract/chunk/embed
    /// pipeline. Called from `ingest()` when `recipe.prebuilt` is set
    /// and the snapshot's `compatible_embedding_model` matches the
    /// locally-loaded model.
    ///
    /// The download lands under
    /// `<index_dir>/_downloads/<corpus_id>.zst` via the existing
    /// `BulkDownloader` (resume-aware, same retry semantics as a
    /// regular bulk-download acquire). The restorer then sha256-checks
    /// it, peeks the manifest, and extracts entries under the parent
    /// of `index_dir` (i.e. `~/.sovereign/`) so the tarball's
    /// `indexes/<id>/` and `enrichment/<id>/` land in the conventional
    /// locations.
    ///
    /// On `compatible_embedding_model` mismatch the caller falls
    /// through *before* reaching this method — by the time we're here
    /// we've committed to restoring.
    pub(crate) async fn try_restore_prebuilt(
        &self,
        recipe: &Recipe,
        prebuilt: &crate::recipe::PrebuiltConfig,
        progress: &Option<ProgressCallback>,
    ) -> Result<IngestResult> {
        use crate::acquirers::bulk_download::BulkDownloader;
        use crate::snapshot_restore::restore_snapshot_archive;

        let corpus_id = recipe.corpus.id.clone();
        let canonical = self.index_dir.join(&corpus_id);
        if canonical.exists() && CorpusIndex::has_committed_data(&canonical) {
            return Err(Error::AlreadyInstalled(format!(
                "corpus '{corpus_id}' already has committed data at {} — \
                 refusing to overwrite with a snapshot restore",
                canonical.display(),
            )));
        }
        // Parent of `index_dir` is the sovereign data root
        // (typically `~/.sovereign/`); that's the directory the
        // restorer extracts into so the archive's `indexes/<id>/`
        // prefix lands at `<root>/indexes/<id>/`.
        let sovereign_data_dir = self
            .index_dir
            .parent()
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "index_dir {} has no parent — cannot determine sovereign data root",
                    self.index_dir.display(),
                ))
            })?
            .to_path_buf();

        let url = format!(
            "https://huggingface.co/datasets/{}/resolve/main/{}",
            prebuilt.hf_repo, prebuilt.hf_filename,
        );
        let download_dir = self.index_dir.join("_downloads");
        std::fs::create_dir_all(&download_dir)?;

        tracing::info!(
            corpus_id = %corpus_id,
            url = %url,
            "ingest: downloading prebuilt snapshot"
        );
        let downloader = BulkDownloader::new(&url, true);
        let archive_path = downloader
            .download(&download_dir, &corpus_id, progress)
            .await?;

        let expected_sha = if prebuilt.sha256.is_empty() {
            None
        } else {
            Some(prebuilt.sha256.as_str())
        };

        let outcome = restore_snapshot_archive(
            &archive_path,
            &sovereign_data_dir,
            &corpus_id,
            expected_sha,
            &self.expected_embedding_model,
            recipe.index.embedding_dimensions,
        )?;

        // The archive is large (multi-GB); delete it once extraction
        // succeeds so we don't double the on-disk footprint.
        if let Err(e) = std::fs::remove_file(&archive_path) {
            tracing::warn!(
                path = %archive_path.display(),
                error = %e,
                "ingest: prebuilt snapshot archive removed failed — index restored OK, archive lingers"
            );
        }

        let index_size_bytes = dir_size_recursive(&outcome.index_dir).unwrap_or(0);
        tracing::info!(
            corpus_id = %corpus_id,
            chunks = outcome.manifest.chunk_count,
            archive_bytes = outcome.archive_size_bytes,
            extracted_bytes = index_size_bytes,
            "ingest: prebuilt snapshot restored"
        );

        Ok(IngestResult {
            corpus_id,
            chunks_created: outcome.manifest.chunk_count,
            index_size_bytes,
            duration_secs: 0,
            docs_skipped: 0,
        })
    }
}
