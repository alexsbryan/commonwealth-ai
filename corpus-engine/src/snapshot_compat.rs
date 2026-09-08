// SPDX-License-Identifier: AGPL-3.0-or-later
//! May a restored snapshot be kept? — the ONE acceptance decision, and the
//! vocabulary it decides in.
//!
//! # Why this is its own module
//!
//! There are TWO restore paths — `CorpusEngine::ingest`'s HuggingFace pull
//! (`engine/ingest_prebuilt.rs`) and the CLI's `snapshot restore --archive`
//! (`snapshot_restore.rs`) — and until 2026-09-07 only the first of them
//! judged anything, so an archive in the wrong embedding space installed
//! silently on the second. Making them one decision (ARCH §10.6) is what this
//! module is; keeping it beside `snapshot.rs` rather than inside it is what
//! §3.1 asks for once a file is already past 1200 lines.
//!
//! # The decision, in order
//!
//! Config first, because config is free and exact: a snapshot that DECLARES an
//! embedder configuration differing from this host's is refusable before a
//! byte is extracted, and the difference can be named. The cosine probe exists
//! only for the archives that declare nothing — which is every snapshot
//! published before the manifest carried the field.
//!
//! # Why a name is not enough
//!
//! `sep` and `wessex-hoard` were both built by `Qwen3-Embedding-0.6B-Q8_0` and
//! their vectors sit 0.66 apart, because one was pooled `Mean` and the other
//! `Last` (note 500f1229). Every verdict here exists because that is possible.

use std::path::Path;

use sovereign_contracts::embed_quirks::EmbedQuirks;

use crate::error::{Error, Result};
use crate::snapshot::SnapshotManifest;

/// Verdict of comparing a snapshot manifest's embedding identity against
/// the locally-loaded model. Dimensions are the hard floor (mismatched
/// dims can't be compared at all); a name mismatch with matching dims is
/// only *plausibly* incompatible — the same model under a different
/// label/quant looks identical here — so the restorer VERIFIES it by
/// re-embedding sample chunks before trusting the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingCompat {
    /// Model name AND dimensions match the local model.
    Exact,
    /// Dimensions match, model name differs, and the manifest's declared
    /// embedder config AGREES with the local one — verify the space by probe.
    NameMismatch,
    /// Dimensions match, model name differs, and the manifest declares no
    /// embedder config at all — so the only evidence available is the probe.
    ///
    /// Every snapshot published before 2026-09-07 is this. Named rather than
    /// folded into [`NameMismatch`] because the two differ in what a FAILING
    /// probe then means: with a declared config that matched, a failure is a
    /// genuine surprise; with no declared config it is the expected outcome
    /// for a snapshot built by an older stack, and the person deserves to be
    /// told which of those they are looking at (ARCH §18.3).
    ConfigUnknown,
    /// Dimensions match but the manifest's declared embedder config DIFFERS
    /// from the local one — pooling, normalization, an instruction, or the
    /// EOS marker. Never usable, and refusable without downloading a byte.
    ///
    /// This is the verdict that would have answered `sep` in 2026-04 instead
    /// of the 0.6822 cosine that took a 93-minute run to produce: sep is
    /// mean-pooled, the stack is last-pooled, and a declared config would have
    /// said so at the manifest (note 500f1229).
    ConfigMismatch,
    /// Dimensions differ — vectors are not comparable; never usable.
    DimsMismatch,
}

impl SnapshotManifest {
    /// Classify this manifest's embedding identity against the
    /// locally-loaded model. Dimensions are the hard floor; a name-only
    /// mismatch returns [`EmbeddingCompat::NameMismatch`] for the caller
    /// to VERIFY by probe rather than trust or reject on the label alone
    /// — model names drift across dir/stem/repo/quant for the same model.
    pub fn check_embedding_compatibility(
        &self,
        local_model: &str,
        local_dimensions: usize,
        local_quirks: Option<&EmbedQuirks>,
    ) -> EmbeddingCompat {
        if self.embedding_dimensions != local_dimensions {
            return EmbeddingCompat::DimsMismatch;
        }
        // Config outranks the name in BOTH directions. A declared config that
        // differs is a refusal even when the names match — the same model file
        // under a different pooling is a different space, which is the whole
        // lesson of `sep` — and a declared config that agrees is what lets a
        // name-only difference stay a probe rather than a rejection.
        match (self.embed_quirks.as_ref(), local_quirks) {
            (Some(declared), Some(local)) if declared != local => {
                return EmbeddingCompat::ConfigMismatch
            }
            _ => {}
        }
        if self.embedding_model == local_model {
            EmbeddingCompat::Exact
        } else if self.embed_quirks.is_some() && local_quirks.is_some() {
            EmbeddingCompat::NameMismatch
        } else {
            EmbeddingCompat::ConfigUnknown
        }
    }
}

/// Say, at publish time, whether this archive will carry an embedder config.
///
/// A snapshot published without one can only ever be checked by probe, which
/// is a real if legal loss of information — so it is announced rather than
/// left to be discovered by a restorer months later (ARCH §18.3).
pub fn log_declared_config(quirks: &Option<EmbedQuirks>) {
    match quirks {
        Some(q) => tracing::debug!(
            pooling = ?q.pooling,
            normalize = ?q.normalize,
            eos = ?q.eos_token,
            "snapshot: recording the embedder config this archive was built with"
        ),
        None => tracing::warn!(
            "snapshot: publishing with NO embedder config declared — a restorer will have \
             only the cosine probe to judge this archive's embedding space by"
        ),
    }
}

// ─── The ONE acceptance decision for a restored snapshot ────────────────────

/// Chunks re-embedded when the manifest cannot settle compatibility on its own.
pub const PREBUILT_PROBE_SAMPLE: usize = 16;

/// Mean-cosine bar a re-embedding must clear against the snapshot's own stored
/// vectors. The same model (any name/quant) re-embeds its own chunks to ≈1.0
/// when the CONFIG matches too; a different pooling lands near 0.70 and a
/// genuinely different model collapses toward 0 — so 0.92 separates "verified
/// compatible" from "would poison the index".
///
/// ONE owner. It was `pub(crate)` in `engine/ingest_prebuilt.rs`, which is why
/// `corpus-mcp/src/serve.rs` carried a hand-copied `0.92` with a comment
/// apologising for it (ARCH §10.6). Import it.
pub const PREBUILT_PROBE_THRESHOLD: f32 = 0.92;

/// Whether a restored snapshot may be kept, and why.
///
/// Four outcomes, not two (ARCH §18.2). `CouldNotJudge` is the one that has to
/// exist: a caller with no embedder cannot run the probe, and treating that as
/// acceptance is how a mean-pooled archive lands silently.
#[derive(Debug, Clone)]
pub enum SnapshotAcceptance {
    Accepted {
        via: &'static str,
        probe_cosine: Option<f32>,
    },
    Refused {
        reason: String,
        probe_cosine: Option<f32>,
    },
    CouldNotJudge {
        reason: String,
    },
}

impl SnapshotAcceptance {
    pub fn is_accepted(&self) -> bool {
        matches!(self, SnapshotAcceptance::Accepted { .. })
    }

    /// One sentence, the same on every restore path.
    pub fn describe(&self) -> String {
        match self {
            SnapshotAcceptance::Accepted {
                via,
                probe_cosine: Some(c),
            } => format!("accepted ({via}, probe cosine {c:.4})"),
            SnapshotAcceptance::Accepted { via, .. } => format!("accepted ({via})"),
            SnapshotAcceptance::Refused {
                reason,
                probe_cosine: Some(c),
            } => format!("REFUSED (probe cosine {c:.4} < {PREBUILT_PROBE_THRESHOLD}): {reason}"),
            SnapshotAcceptance::Refused { reason, .. } => format!("REFUSED: {reason}"),
            SnapshotAcceptance::CouldNotJudge { reason } => format!("COULD-NOT-JUDGE: {reason}"),
        }
    }
}

/// Re-embed a sample of an index's own chunks and cosine them against the
/// stored vectors. The empirical test of "is this the same embedding space".
///
/// Lifted out of `CorpusEngine::probe_embedding_space` so the decision below
/// has ONE implementation reachable without an engine — the CLI's
/// `snapshot restore --archive` has no `CorpusEngine` and, until 2026-09-07,
/// therefore had no probe at all.
pub async fn probe_embedding_space_at(
    index_dir: &Path,
    embed: &crate::types::EmbedFn,
    batch_embed: Option<&crate::types::BatchEmbedFn>,
) -> Result<f32> {
    let index = crate::CorpusIndex::open(index_dir).await?;
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

    // Pair (stored vector, chunk text) in one pass so the two lists stay
    // index-aligned even when some sampled ids carry no text.
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

    // The DOCUMENT embedder — the same side ingest used to produce these
    // vectors, so the comparison is like-with-like.
    let local: Vec<Vec<f32>> = if let Some(batch) = batch_embed {
        (batch)(&texts).await?
    } else {
        let mut v = Vec::with_capacity(texts.len());
        for t in &texts {
            v.push((embed)(t).await?);
        }
        v
    };

    let mut sims = Vec::new();
    for (l, s) in local.iter().zip(stored.iter()) {
        if l.len() == s.len() && !l.is_empty() {
            sims.push(crate::engine::ingest_prebuilt::cosine(l, s));
        }
    }
    if sims.is_empty() {
        return Err(Error::InvalidInput(
            "no comparable probe vectors (dimension mismatch on every sample)".to_string(),
        ));
    }
    Ok(sims.iter().sum::<f32>() / sims.len() as f32)
}

/// May this restored snapshot be kept?
///
/// THE one decider, called by BOTH restore paths — `CorpusEngine::ingest`'s
/// HuggingFace pull and the CLI's `snapshot restore --archive <path>`. They
/// used to differ: the pull probed and the local path did not, so a
/// mean-pooled archive handed to `--archive` installed silently, which is the
/// `sep` failure on the path with no deadline to catch it (ARCH §10.6).
///
/// The order is config first, probe second, because config is free and exact:
/// a declared mismatch is refusable before anything is re-embedded, and the
/// probe exists only for the archives that declare nothing.
pub async fn judge_restored_snapshot(
    manifest: &SnapshotManifest,
    index_dir: &Path,
    compat: EmbeddingCompat,
    local_model: &str,
    embed: Option<&crate::types::EmbedFn>,
    batch_embed: Option<&crate::types::BatchEmbedFn>,
) -> SnapshotAcceptance {
    match compat {
        EmbeddingCompat::DimsMismatch => SnapshotAcceptance::Refused {
            reason: format!(
                "snapshot is {}-dim and this host's `{local_model}` is not; vectors are not \
                 comparable at all",
                manifest.embedding_dimensions
            ),
            probe_cosine: None,
        },
        EmbeddingCompat::ConfigMismatch => SnapshotAcceptance::Refused {
            reason: config_mismatch_sentence(manifest),
            probe_cosine: None,
        },
        EmbeddingCompat::Exact => SnapshotAcceptance::Accepted {
            via: "model name and dimensions match",
            probe_cosine: None,
        },
        // The name differs and the manifest could not settle it. Only the
        // vectors can answer now.
        EmbeddingCompat::NameMismatch | EmbeddingCompat::ConfigUnknown => {
            let Some(embed) = embed else {
                return SnapshotAcceptance::CouldNotJudge {
                    reason: format!(
                        "snapshot names `{}` and this host names `{local_model}`; the \
                         embedding space can only be settled by re-embedding a sample, and no \
                         embedder was supplied to this restore path",
                        manifest.embedding_model
                    ),
                };
            };
            match probe_embedding_space_at(index_dir, embed, batch_embed).await {
                Ok(score) if score >= PREBUILT_PROBE_THRESHOLD => SnapshotAcceptance::Accepted {
                    via: "embedding-space probe",
                    probe_cosine: Some(score),
                },
                Ok(score) => SnapshotAcceptance::Refused {
                    reason: probe_failure_cause(manifest),
                    probe_cosine: Some(score),
                },
                Err(e) => SnapshotAcceptance::CouldNotJudge {
                    reason: format!("the embedding-space probe could not run: {e}"),
                },
            }
        }
    }
}

/// Why a probe in the failing range is almost never a broken embedder.
///
/// A bare cosine sends the reader hunting through their endpoint. It is
/// usually the archive: `sep` and `wikipedia` were published mean-pooled under
/// a last-pooled stack and score ≈0.70 against any current host (note
/// 500f1229). Name the degradation (ARCH §18.3).
fn probe_failure_cause(manifest: &SnapshotManifest) -> String {
    if manifest.embed_quirks.is_none() {
        "the snapshot declares no embedder config — it was published before that manifest \
         field existed, so pooling could not be compared before downloading. A different \
         POOLING is the usual cause of a score in this range, and no re-embedding on this \
         host can fix it: the corpus must be re-published from the current stack."
            .to_string()
    } else {
        "the snapshot's declared embedder config MATCHED this host, so a failing probe is a \
         genuine surprise — investigate the endpoint rather than re-publishing."
            .to_string()
    }
}

/// Both configurations, side by side, so the difference is readable.
fn config_mismatch_sentence(manifest: &SnapshotManifest) -> String {
    match manifest.embed_quirks.as_ref() {
        Some(d) => format!(
            "snapshot declares pooling={:?} normalize={:?} eos={:?}, which differs from this \
             host's embedder. The same model NAME is not the same embedding space — `sep` and \
             `wessex-hoard` were both built by Qwen3-Embedding-0.6B-Q8_0 and sit 0.66 apart \
             because one is mean-pooled. This corpus must be re-published from the current stack.",
            d.pooling, d.normalize, d.eos_token
        ),
        None => "declared embedder config differs from this host's".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::tests::write_fake_index_dir;
    use crate::snapshot::{publish_snapshot, read_manifest_from_archive, PublishOptions};

    /// The manifest every verdict test starts from. Local to this module
    /// rather than shared with `snapshot.rs`'s: these tests are about the
    /// COMPAT fields, and a fixture that drifts to suit a publish test would
    /// quietly change what they assert.
    fn sample_manifest() -> SnapshotManifest {
        SnapshotManifest::new(
            "wikipedia",
            "Wikipedia (English)",
            "wikipedia-qwen3-embedding-0.6b-2026-05-12",
            "qwen3-embedding-0.6b",
            1024,
            1_847_442,
            true,
            "sovereign-cli/0.1.0",
        )
    }

    #[test]
    fn embedding_compatibility_flags_model_name_mismatch() {
        let m = sample_manifest();
        // Same dims (1024), different name, and this manifest declares no
        // embedder config → ConfigUnknown: verify by probe, do NOT reject on
        // the label alone, and say that the probe is the only evidence there
        // is.
        assert_eq!(
            m.check_embedding_compatibility("jina-v2-en", 1024, None),
            EmbeddingCompat::ConfigUnknown
        );
    }

    /// A DECLARED config that differs is a refusal before a byte moves —
    /// even when the model name matches exactly. This is the `sep` case:
    /// same model file, `Mean` pooling against a `Last`-pooled stack, 0.66
    /// cosine apart (note 500f1229). Today it costs a download, an extract
    /// and a 16-chunk probe to learn that.
    #[test]
    fn embedding_compatibility_refuses_a_declared_config_mismatch() {
        let mut mean_pooled = EmbedQuirks::qwen3_embedding();
        mean_pooled.pooling = sovereign_contracts::oicp::PoolingStrategy::Mean;
        let m = sample_manifest().with_embed_quirks(mean_pooled);
        let local = EmbedQuirks::qwen3_embedding();
        assert_eq!(
            m.check_embedding_compatibility("qwen3-embedding-0.6b", 1024, Some(&local)),
            EmbeddingCompat::ConfigMismatch,
            "a name-identical, mean-pooled snapshot must be refused on the config"
        );
    }

    /// Two declared configs that agree leave a name difference exactly where
    /// it was: a probe, not a rejection.
    #[test]
    fn embedding_compatibility_keeps_a_name_difference_probeable_when_configs_agree() {
        let local = EmbedQuirks::qwen3_embedding();
        let m = sample_manifest().with_embed_quirks(local.clone());
        assert_eq!(
            m.check_embedding_compatibility("qwen-embedding-0.6b", 1024, Some(&local)),
            EmbeddingCompat::NameMismatch
        );
    }

    /// A local config the manifest cannot be compared against is still
    /// ConfigUnknown — the absence is the manifest's, and it is reported as
    /// such rather than being read as agreement (ARCH §18.3).
    #[test]
    fn embedding_compatibility_reports_an_undeclared_config_as_unknown() {
        let local = EmbedQuirks::qwen3_embedding();
        let m = sample_manifest();
        assert_eq!(
            m.check_embedding_compatibility("qwen-embedding-0.6b", 1024, Some(&local)),
            EmbeddingCompat::ConfigUnknown
        );
    }

    #[test]
    fn embedding_compatibility_blocks_dimension_mismatch() {
        let m = sample_manifest();
        // Different dims → DimsMismatch: the hard floor, never usable.
        assert_eq!(
            m.check_embedding_compatibility("qwen3-embedding-0.6b", 768, None),
            EmbeddingCompat::DimsMismatch
        );
    }

    #[test]
    fn embedding_compatibility_accepts_exact_match() {
        let m = sample_manifest();
        assert_eq!(
            m.check_embedding_compatibility("qwen3-embedding-0.6b", 1024, None),
            EmbeddingCompat::Exact
        );
    }

    #[test]
    fn manifest_roundtrip_preserves_the_embedder_config() {
        let m = sample_manifest().with_embed_quirks(EmbedQuirks::qwen3_embedding());
        let parsed =
            SnapshotManifest::from_json_bytes(m.to_json_pretty().unwrap().as_bytes()).unwrap();
        assert_eq!(parsed.embed_quirks, Some(EmbedQuirks::qwen3_embedding()));
    }

    /// Every snapshot published before 2026-09-07 has no `embed_quirks` key
    /// at all. It must still parse.
    #[test]
    fn manifest_without_an_embedder_config_still_parses() {
        let m = sample_manifest();
        let mut v: serde_json::Value = serde_json::from_str(&m.to_json_pretty().unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("embed_quirks");
        let parsed = SnapshotManifest::from_json_bytes(v.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.embed_quirks, None);
    }

    /// The publisher's declaration survives into the archive, and a restorer
    /// with a DIFFERENT config refuses it from the manifest alone — no
    /// download of the vectors, no probe. This is the producer that gives
    /// `EmbeddingCompat::ConfigMismatch` something to fire on; without it the
    /// verdict is a gate with no input that can make it fail (ARCH §18.1).
    #[tokio::test]
    async fn published_manifest_carries_the_embedder_config_and_a_mismatch_is_refusable() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/wikitest");
        let output_path = tmp.path().join("out.tar.zst");
        write_fake_index_dir(&index_dir, "wikitest", "qwen3-embedding-0.6b", 1024);

        // Publish as a MEAN-pooled corpus — `sep`'s real situation.
        let mut published_with = EmbedQuirks::qwen3_embedding();
        published_with.pooling = sovereign_contracts::oicp::PoolingStrategy::Mean;

        let outcome = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: None,
            output_path: output_path.clone(),
            snapshot_id: "wikitest-2026-09-07".into(),
            chunk_count: 42,
            residual_gap_pct: None,
            notes: None,
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
            sibling_index_dirs: Vec::new(),
            embed_quirks: Some(published_with.clone()),
        })
        .await
        .unwrap();

        let read_back = read_manifest_from_archive(&output_path).unwrap();
        assert_eq!(
            read_back.embed_quirks.as_ref(),
            Some(&published_with),
            "the config the publisher declared must survive into the archive"
        );
        assert_eq!(
            outcome.manifest.embed_quirks.as_ref(),
            Some(&published_with)
        );

        // A last-pooled restorer refuses it on the manifest, by name.
        let local = EmbedQuirks::qwen3_embedding();
        assert_eq!(
            read_back.check_embedding_compatibility("qwen3-embedding-0.6b", 1024, Some(&local)),
            EmbeddingCompat::ConfigMismatch
        );
        // ... and the same restorer accepts its own configuration.
        assert_eq!(
            read_back.check_embedding_compatibility(
                "qwen3-embedding-0.6b",
                1024,
                Some(&published_with)
            ),
            EmbeddingCompat::Exact
        );
    }
}
