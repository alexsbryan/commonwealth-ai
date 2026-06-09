// SPDX-License-Identifier: AGPL-3.0-or-later
//! `CorpusIndexChecker` — health-checks every installed corpus index.
//!
//! Checks:
//! 1. Embed model / dimension match against the active slot.
//! 2. Partial ingestion (chunks_indexed < chunks_expected).
//! 3. FTS sync (fts_doc_count vs chunk_count).
//! 4. Corrupt embeddings (all-zero or non-finite sample vectors).
//! 5. Update available (manifest URL check).

use std::sync::Arc;

use corpus_engine::CorpusEngine;
use sovereign_core::error::{Error, Result};
use sovereign_core::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, RepairKind, RepairOutcome, UpdateDelta,
};

/// Convert a corpus-engine error to a sovereign-core error.
fn ce(e: corpus_engine::Error) -> Error {
    Error::Other(Box::new(e))
}

/// Configuration for the active embedding slot.
/// Compared against each `IndexInfo` to detect model-drift.
#[derive(Debug, Clone)]
pub struct EmbedSlotConfig {
    /// Model identifier string (e.g. `"qwen3-embedding-0.6b"`).
    pub model_id: String,
    /// Number of float32 dimensions this model produces.
    pub output_dims: usize,
}

// ─── CorpusIndexChecker ──────────────────────────────────────────────────────

pub struct CorpusIndexChecker {
    engine: Arc<CorpusEngine>,
    embed_slot: Arc<tokio::sync::RwLock<EmbedSlotConfig>>,
}

impl CorpusIndexChecker {
    pub fn new(
        engine: Arc<CorpusEngine>,
        embed_slot: Arc<tokio::sync::RwLock<EmbedSlotConfig>>,
    ) -> Self {
        Self { engine, embed_slot }
    }
}

impl HealthCheckable for CorpusIndexChecker {
    fn component(&self) -> Component {
        // We report issues per-corpus inside the report's issue list;
        // the top-level component is a placeholder.
        Component::CorpusIndex("*".into())
    }

    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>
    {
        Box::pin(async move {
            let slot = self.embed_slot.read().await;
            let active_model = slot.model_id.clone();
            let active_dims = slot.output_dims;
            drop(slot);

            let indexes = self.engine.installed_indexes().await.map_err(ce)?;
            let mut issues = Vec::new();

            for info in &indexes {
                let corpus_id = info.corpus_id.clone();

                // ── 1. Embed model / dimension mismatch ──────────────────────
                if !info.embedding_model.is_empty()
                    && (info.embedding_model != active_model
                        || info.embedding_dimensions != active_dims)
                {
                    issues.push(HealthIssue::EmbedModelMismatch {
                        corpus_id: corpus_id.clone(),
                        index_model: info.embedding_model.clone(),
                        active_model: active_model.clone(),
                        index_dims: info.embedding_dimensions,
                        active_dims,
                    });
                }

                // ── 2. Partial ingestion ─────────────────────────────────────
                if let Some(expected) = info.chunks_expected {
                    if info.chunk_count < expected {
                        issues.push(HealthIssue::PartialIngestion {
                            corpus_id: corpus_id.clone(),
                            chunks_indexed: info.chunk_count,
                            chunks_expected: expected,
                            resume_from: info.resume_from.clone(),
                        });
                    }
                }

                // ── 3. Corrupt embeddings (sample check) ─────────────────────
                if let Ok(index) = self.engine.open_index_for_corpus(&corpus_id).await {
                    if let Ok(samples) = index.sample_embeddings(32).await {
                        let bad: Vec<u64> = samples
                            .iter()
                            .filter(|(_, v)| is_pathological(v))
                            .map(|(id, _)| *id)
                            .collect();
                        if !bad.is_empty() {
                            issues.push(HealthIssue::CorruptEmbeddings {
                                corpus_id: corpus_id.clone(),
                                bad_chunk_ids: bad,
                            });
                        }
                    }

                    // ── 4. FTS desync ────────────────────────────────────────
                    let chunk_count = info.chunk_count;
                    if let Ok(fts_count) = index.fts_doc_count().await {
                        // Allow small delta (< 1%) for in-flight writes.
                        let delta = chunk_count.abs_diff(fts_count);
                        let threshold = (chunk_count / 100).max(1);
                        if delta > threshold {
                            issues.push(HealthIssue::FtsDesync {
                                corpus_id: corpus_id.clone(),
                                fts_count,
                                chunk_count,
                            });
                        }
                    }

                    // ── 5. Update available ───────────────────────────────────
                    if let Some(url) = &info.update_manifest_url {
                        if let Ok(Some(latest)) = fetch_manifest_version(url).await {
                            let current = info
                                .source_version
                                .clone()
                                .unwrap_or_else(|| "unknown".into());
                            if latest != current {
                                issues.push(HealthIssue::UpdateAvailable {
                                    corpus_id: corpus_id.clone(),
                                    current_version: current,
                                    latest_version: latest,
                                    delta: UpdateDelta {
                                        new_documents: 0,
                                        updated_documents: 0,
                                        deleted_documents: 0,
                                    },
                                });
                            }
                        }
                    }
                }
            }

            // Use first corpus's id as the component (or "*" for multi-corpus)
            let component = if indexes.len() == 1 {
                Component::CorpusIndex(indexes[0].corpus_id.clone())
            } else {
                Component::CorpusIndex("*".into())
            };
            Ok(HealthReport::from_issues(component, issues))
        })
    }

    fn repair(
        &self,
        issue: &HealthIssue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>>
    {
        let issue = issue.clone();
        Box::pin(async move {
            match &issue {
                HealthIssue::FtsDesync { corpus_id, .. } => {
                    match self.engine.open_index_for_corpus(corpus_id).await {
                        Ok(index) => {
                            index.rebuild_fts().await.map_err(ce)?;
                            Ok(RepairOutcome::Resolved)
                        }
                        Err(_) => Ok(RepairOutcome::Failed {
                            reason: format!("Could not open index for {corpus_id}"),
                        }),
                    }
                }
                HealthIssue::CorruptEmbeddings {
                    corpus_id,
                    bad_chunk_ids,
                } => {
                    let embed_fn = self.engine.embed_fn();
                    match self.engine.open_index_for_corpus(corpus_id).await {
                        Ok(index) => {
                            index
                                .re_embed_chunks(bad_chunk_ids, &embed_fn)
                                .await
                                .map_err(ce)?;
                            Ok(RepairOutcome::Resolved)
                        }
                        Err(_) => Ok(RepairOutcome::Failed {
                            reason: format!("Could not open index for {corpus_id}"),
                        }),
                    }
                }
                HealthIssue::PartialIngestion {
                    corpus_id,
                    resume_from,
                    ..
                } => {
                    if let Some(cursor) = resume_from {
                        Ok(RepairOutcome::NeedsUserDecision {
                            question: format!(
                                "Corpus `{corpus_id}` has incomplete ingestion. Resume from `{cursor}`?"
                            ),
                            options: vec![
                                sovereign_core::health::UserOption {
                                    kind: RepairKind::ResumeIngestion,
                                    label: "Resume ingestion".into(),
                                    description: "Continue ingesting from the last checkpoint.".into(),
                                },
                                sovereign_core::health::UserOption {
                                    kind: RepairKind::Dismiss,
                                    label: "Dismiss".into(),
                                    description: "Ignore — the partial index will still be searchable.".into(),
                                },
                            ],
                            consequence: "Resuming will re-read and embed the remaining chunks."
                                .into(),
                        })
                    } else {
                        Err(Error::RepairNotSupported)
                    }
                }
                HealthIssue::EmbedModelMismatch { corpus_id, .. } => {
                    Ok(RepairOutcome::NeedsUserDecision {
                        question: format!(
                            "Corpus `{corpus_id}` was built with a different embed model. \
                             Re-embed the entire corpus with the current model?"
                        ),
                        options: vec![
                            sovereign_core::health::UserOption {
                                kind: RepairKind::ReEmbed,
                                label: "Re-embed corpus".into(),
                                description: "Recompute all embeddings with the active model. \
                                              This may take a long time."
                                    .into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description: "Search quality will be degraded.".into(),
                            },
                        ],
                        consequence: "The entire corpus will be re-embedded.".into(),
                    })
                }
                _ => Err(Error::RepairNotSupported),
            }
        })
    }

    fn can_repair_autonomously(&self, issue: &HealthIssue) -> bool {
        // FTS rebuilds are safe but only "fast" for small corpora.
        // On a 500 K-chunk Wikipedia index a content-FTS rebuild
        // takes ~3 minutes, which is the kind of work that absolutely
        // must not run silently in the background while the user is
        // trying to chat. Threshold below is the rebuild *workload*
        // (delta between chunk_count and fts_count) — small drift
        // gets repaired silently, large drift surfaces as a user
        // decision via the standard `maybe_surface_decision` path.
        match issue {
            HealthIssue::FtsDesync {
                chunk_count,
                fts_count,
                ..
            } => {
                let delta = chunk_count.abs_diff(*fts_count);
                delta <= AUTO_FTS_REPAIR_MAX_DELTA
            }
            _ => false,
        }
    }
}

/// Maximum number of chunks a silent FTS rebuild may touch. Above
/// this, the rebuild is surfaced to the user instead of running
/// autonomously. 5 K is empirically the boundary at which a tantivy
/// FTS build crosses ~5 s on this hardware — fast enough that a user
/// with the app open won't notice, slow enough that anything larger
/// genuinely competes with foreground work.
const AUTO_FTS_REPAIR_MAX_DELTA: u64 = 5_000;

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// True when a vector is all-zero, NaN, or contains Inf.
fn is_pathological(v: &[f32]) -> bool {
    if v.is_empty() {
        return true;
    }
    if v.iter().all(|&x| x == 0.0) {
        return true;
    }
    v.iter().any(|x| !x.is_finite())
}

/// Fetch just the `version` field from a manifest URL.
/// Network errors → `Ok(None)` (silently skipped — no false alarms).
async fn fetch_manifest_version(url: &str) -> Result<Option<String>> {
    let response = match reqwest::get(url).await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let text = match response.text().await {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(v.get("version").and_then(|v| v.as_str()).map(String::from))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pathological_all_zero() {
        assert!(is_pathological(&[0.0, 0.0, 0.0]));
    }

    #[test]
    fn is_pathological_nan() {
        assert!(is_pathological(&[1.0, f32::NAN, 0.5]));
    }

    #[test]
    fn is_pathological_inf() {
        assert!(is_pathological(&[1.0, f32::INFINITY, 0.5]));
    }

    #[test]
    fn is_pathological_valid() {
        assert!(!is_pathological(&[0.1, -0.5, 0.3]));
    }
}
