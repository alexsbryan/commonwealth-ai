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
use crate::snapshot::EmbeddingCompat;
use crate::types::IngestResult;

/// Sample size + mean-cosine threshold for the name-mismatch embedding
/// probe. The same model (any name/quant) re-embeds its own chunks to
/// ≈1.0; a genuinely different model collapses toward 0 — so 0.92 cleanly
/// separates "verified compatible" from "would poison the index".
const PREBUILT_PROBE_SAMPLE: usize = 16;
const PREBUILT_PROBE_THRESHOLD: f32 = 0.92;

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

        // The model NAME differed from the snapshot's. Names are unreliable
        // (dir/stem/repo/quant all drift for the same model), so VERIFY the
        // embedding space empirically before trusting the vectors: re-embed
        // a sample of the snapshot's own chunks with the local document
        // embedder and compare to their stored vectors.
        if outcome.embedding_compat == EmbeddingCompat::NameMismatch {
            let forced = std::env::var("SOVEREIGN_FORCE_PREBUILT")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false);
            if forced {
                tracing::warn!(
                    corpus_id = %corpus_id,
                    snapshot_model = %outcome.manifest.embedding_model,
                    local_model = %self.expected_embedding_model,
                    "ingest: prebuilt model-name mismatch — SOVEREIGN_FORCE_PREBUILT set, skipping the embedding-space probe"
                );
            } else {
                let verdict = self.probe_embedding_space(&outcome.index_dir).await;
                let accepted = matches!(&verdict, Ok(score) if *score >= PREBUILT_PROBE_THRESHOLD);
                if !accepted {
                    match &verdict {
                        Ok(score) => tracing::warn!(
                            corpus_id = %corpus_id,
                            snapshot_model = %outcome.manifest.embedding_model,
                            local_model = %self.expected_embedding_model,
                            probe_cosine = score,
                            threshold = PREBUILT_PROBE_THRESHOLD,
                            "ingest: embedding-space probe FAILED — discarding snapshot, full ingest"
                        ),
                        Err(e) => tracing::warn!(
                            corpus_id = %corpus_id,
                            error = %e,
                            "ingest: embedding-space probe could not run on a name-mismatched snapshot — discarding, full ingest"
                        ),
                    }
                    let _ = std::fs::remove_dir_all(&outcome.index_dir);
                    if let Some(enr) = &outcome.enrichment_dir {
                        let _ = std::fs::remove_dir_all(enr);
                    }
                    let _ = std::fs::remove_file(&archive_path);
                    return Ok(None);
                }
                if let Ok(score) = verdict {
                    tracing::info!(
                        corpus_id = %corpus_id,
                        snapshot_model = %outcome.manifest.embedding_model,
                        local_model = %self.expected_embedding_model,
                        probe_cosine = score,
                        "ingest: embedding-space probe PASSED — accepting name-mismatched snapshot"
                    );
                }
            }
        }

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

    /// Empirical embedding-space compatibility probe. Re-embeds a sample
    /// of the restored index's own chunks with the LOCAL document embedder
    /// and returns the mean cosine against their stored vectors: ≈1.0 when
    /// the spaces match (same model, any name/quant), collapsing toward 0
    /// for a genuinely different model. Errors when there's nothing to
    /// probe (caller treats that as "don't trust").
    async fn probe_embedding_space(&self, index_dir: &Path) -> Result<f32> {
        let index = CorpusIndex::open(index_dir).await?;
        let sample = index.sample_embeddings(PREBUILT_PROBE_SAMPLE).await?;
        if sample.is_empty() {
            return Err(Error::InvalidInput(
                "prebuilt snapshot has no chunk vectors to probe".to_string(),
            ));
        }
        let ids: Vec<u64> = sample.iter().map(|(id, _)| *id).collect();
        let chunks = index.get_chunks(&ids).await?;
        let text_by_id: std::collections::HashMap<u64, &str> =
            chunks.iter().map(|c| (c.id, c.content.as_str())).collect();

        // Pair (stored vector, chunk text) in one pass so the two lists
        // stay index-aligned even when some sampled ids carry no text.
        let mut stored: Vec<&Vec<f32>> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        for (id, vec) in &sample {
            if let Some(t) = text_by_id.get(id) {
                stored.push(vec);
                texts.push((*t).to_string());
            }
        }
        if texts.is_empty() {
            return Err(Error::InvalidInput(
                "prebuilt snapshot chunks carry no text to re-embed".to_string(),
            ));
        }

        // Re-embed via the DOCUMENT embedder (the same path ingest used to
        // produce these vectors) so the comparison is like-with-like.
        let local: Vec<Vec<f32>> = if let Some(batch) = self.batch_embed.as_ref() {
            (batch)(&texts).await?
        } else {
            let mut v = Vec::with_capacity(texts.len());
            for t in &texts {
                v.push((self.embed)(t).await?);
            }
            v
        };

        let mut sims = Vec::new();
        for (l, s) in local.iter().zip(stored.iter()) {
            if l.len() == s.len() && !l.is_empty() {
                sims.push(cosine(l, s));
            }
        }
        if sims.is_empty() {
            return Err(Error::InvalidInput(
                "no comparable probe vectors (dimension mismatch on every sample)".to_string(),
            ));
        }
        Ok(sims.iter().sum::<f32>() / sims.len() as f32)
    }
}

/// Cosine similarity of two equal-length vectors. Scale-invariant, so
/// stored-vs-re-embedded vectors compare correctly regardless of any
/// per-vector normalization difference.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
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
