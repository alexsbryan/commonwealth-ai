// SPDX-License-Identifier: AGPL-3.0-or-later
//! `try_restore_prebuilt` — extracted out of `engine::ingest`.
//!
//! Pulled into its own module so the `ingest()` orchestrator stays
//! focused on the acquire/extract/chunk/embed pipeline. Shape is
//! behaviour-preserving: same signature, same error semantics, same
//! tracing events as before the move.

use std::path::Path;

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
    /// of `index_dir` (i.e. `~/.svrnmesh/`) so the tarball's
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
    ) -> Result<Option<IngestResult>> {
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
        // (typically `~/.svrnmesh/`); that's the directory the
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

        // Extract. Dimensions are the hard floor inside the restorer — a
        // dim mismatch returns `SnapshotIncompatible`, which we treat as
        // "fall through to a full ingest with the local model" (Ok(None)).
        let outcome = match restore_snapshot_archive(
            &archive_path,
            &sovereign_data_dir,
            &corpus_id,
            expected_sha,
            &self.expected_embedding_model,
            recipe.index.embedding_dimensions,
            self.expected_embed_quirks.as_ref(),
        ) {
            Ok(o) => o,
            Err(Error::SnapshotIncompatible(reason)) => {
                tracing::warn!(
                    corpus_id = %corpus_id,
                    reason = %reason,
                    "ingest: prebuilt snapshot incompatible — falling through to full ingest"
                );
                let _ = std::fs::remove_file(&archive_path);
                return Ok(None);
            }
            Err(e) => return Err(e),
        };

        // MAY WE KEEP IT? One decider, shared with the CLI's
        // `snapshot restore --archive` path (ARCH §10.6). Until 2026-09-07
        // this block WAS the decision and the local path had none, so an
        // archive in the wrong embedding space installed silently there.
        let forced = std::env::var("SOVEREIGN_FORCE_PREBUILT")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        let acceptance = if forced {
            tracing::warn!(
                corpus_id = %corpus_id,
                snapshot_model = %outcome.manifest.embedding_model,
                local_model = %self.expected_embedding_model,
                "ingest: SOVEREIGN_FORCE_PREBUILT set — accepting the snapshot WITHOUT judging its embedding space"
            );
            crate::snapshot::SnapshotAcceptance::Accepted {
                via: "SOVEREIGN_FORCE_PREBUILT",
                probe_cosine: None,
            }
        } else {
            crate::snapshot::judge_restored_snapshot(
                &outcome.manifest,
                &outcome.index_dir,
                outcome.embedding_compat,
                &self.expected_embedding_model,
                Some(&self.embed),
                self.batch_embed.as_ref(),
            )
            .await
        };

        // A snapshot that is not ACCEPTED is discarded — including
        // `CouldNotJudge`, because an unjudged archive is exactly the silent
        // substitution §18.3 forbids, and falling through to a full ingest is
        // the honest outcome (slow, correct) rather than the fast wrong one.
        if !acceptance.is_accepted() {
            tracing::warn!(
                corpus_id = %corpus_id,
                snapshot_model = %outcome.manifest.embedding_model,
                local_model = %self.expected_embedding_model,
                declared_embed_config = outcome.manifest.embed_quirks.is_some(),
                verdict = %acceptance.describe(),
                "ingest: prebuilt snapshot NOT accepted — discarding, full ingest"
            );
            let _ = std::fs::remove_dir_all(&outcome.index_dir);
            if let Some(enr) = &outcome.enrichment_dir {
                let _ = std::fs::remove_dir_all(enr);
            }
            let _ = std::fs::remove_file(&archive_path);
            return Ok(None);
        }
        tracing::info!(
            corpus_id = %corpus_id,
            snapshot_model = %outcome.manifest.embedding_model,
            local_model = %self.expected_embedding_model,
            verdict = %acceptance.describe(),
            "ingest: prebuilt snapshot accepted"
        );

        // The archive is large (multi-GB); delete it once we've committed
        // to keeping the restored index.
        if let Err(e) = std::fs::remove_file(&archive_path) {
            tracing::warn!(
                path = %archive_path.display(),
                error = %e,
                "ingest: prebuilt snapshot archive remove failed — index restored OK, archive lingers"
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

        Ok(Some(IngestResult {
            corpus_id,
            chunks_created: outcome.manifest.chunk_count,
            index_size_bytes,
            duration_secs: 0,
            docs_skipped: 0,
        }))
    }
}

/// Cosine similarity of two equal-length vectors. Scale-invariant, so
/// stored-vs-re-embedded vectors compare correctly regardless of any
/// per-vector normalization difference.
/// One implementation, used by the shared restore decider in
/// `crate::snapshot::probe_embedding_space_at` as well as here (ARCH §10.6).
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::cosine;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![0.1, 0.2, 0.3, 0.4];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_scale_invariant() {
        // Same direction, different magnitude → still ≈1.0. Stored and
        // re-embedded vectors can differ in norm; the probe must not care.
        assert!((cosine(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_vector_is_zero() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
