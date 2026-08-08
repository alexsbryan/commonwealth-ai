// SPDX-License-Identifier: AGPL-3.0-or-later
//! `EnrichmentChecker` — health-checks field model enrichment for every corpus.
//!
//! Checks for enrichment completeness based on the `field_skeleton.json`
//! artifact and the `_enrichment_checkpoint.json` resume state.
//!
//! It looks at TWO sets of directories, because the corpora that failed
//! hardest are the ones the normal listing cannot see:
//!
//! 1. `installed_indexes()` — corpora whose ingest finished. Those that asked
//!    for enrichment and have no field-model tables raise
//!    `LowEnrichmentCoverage`; those with a live checkpoint raise
//!    `StaleEnrichment`. Each is opened at the path the listing reported
//!    (`CorpusIndex::open(&info.path)`, the same resolution
//!    `CorpusEngine::enriched_corpus_ids` uses) rather than at
//!    `index_dir/<corpus_id>`, which cannot reach a partition directory.
//! 2. `incomplete_ingests()` — directories that listing deliberately drops
//!    because `ingestion_in_progress` is still `true`. An ingest that dies
//!    inside its enrichment phase leaves exactly one of these, and until
//!    2026-08-07 nothing on the machine reported it: the corpus read as
//!    absent (`docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §3).

use std::sync::Arc;

use corpus_engine::{CorpusEngine, CorpusIndex};
use sovereign_core::error::{Error, Result};
use sovereign_core::health::{
    Component, HealthCheckable, HealthIssue, HealthReport, RepairKind, RepairOutcome,
};

// ─── EnrichmentChecker ───────────────────────────────────────────────────────

pub struct EnrichmentChecker {
    engine: Arc<CorpusEngine>,
}

impl EnrichmentChecker {
    pub fn new(engine: Arc<CorpusEngine>) -> Self {
        Self { engine }
    }
}

// ── HealthCheckable impl ──────────────────────────────────────────────────────

impl HealthCheckable for EnrichmentChecker {
    fn component(&self) -> Component {
        Component::Enrichment("*".into())
    }

    fn check(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<HealthReport>> + Send + '_>>
    {
        Box::pin(async move {
            let indexes = self
                .engine
                .installed_indexes()
                .await
                .map_err(|e| Error::Other(Box::new(e)))?;

            let mut issues = Vec::new();

            for info in &indexes {
                // `enrichment_requested` is the recipe's ASK, stamped at the
                // entry of ingest's enrichment block — not a completion flag.
                // That is what makes the rest of this loop reachable: a
                // corpus that asked and then failed still arrives here.
                // Until 2026-08-07 this read a field nothing ever wrote
                // `true`, so this `continue` fired for every corpus, always,
                // and no issue below could be reported for any input
                // (`docs/TRACE_ENRICHMENT_ENABLED_FLAG.md` §4).
                if !info.enrichment_requested {
                    continue;
                }
                let corpus_id = info.corpus_id.clone();

                // Open the directory the listing actually found, NOT
                // `index_dir/<corpus_id>`. Same resolution
                // `CorpusEngine::enriched_corpus_ids` uses — one decider
                // (§10.6) — and the difference is not cosmetic: an index
                // whose directory is `<corpus_id>-partition-<node>/` because
                // promotion never ran is listed here with that path, while
                // `open_index_for_corpus` joins the canonical name and finds
                // nothing.
                let index = match CorpusIndex::open(&info.path).await {
                    Ok(index) => index,
                    Err(e) => {
                        // Report the absence, never default past it (§18.3).
                        // The previous `if let Ok(index)` swallowed this arm
                        // entirely, so a corpus the checker could not open
                        // contributed no issue and no log line — the report
                        // came back "All checks passed" for a corpus nobody
                        // had actually checked.
                        tracing::warn!(
                            corpus = %corpus_id,
                            path = %info.path.display(),
                            error = %e,
                            "enrichment check: could not open a corpus that \
                             requested enrichment — it is neither confirmed \
                             enriched nor reported unenriched"
                        );
                        continue;
                    }
                };

                // Check if field model tables exist.
                let has_field_model = index.has_field_model_tables().await;

                if !has_field_model {
                    // No field model — enrichment was enabled but never completed.
                    issues.push(HealthIssue::LowEnrichmentCoverage {
                        corpus_id: corpus_id.clone(),
                        enriched_chunks: 0,
                        total_chunks: info.chunk_count,
                        coverage_pct: 0.0,
                        threshold_pct: 80.0,
                    });
                    continue;
                }

                // Check for interrupted enrichment (checkpoint file exists).
                let checkpoint_path = index.path().join("_enrichment_checkpoint.json");
                if checkpoint_path.exists() {
                    issues.push(HealthIssue::StaleEnrichment {
                        corpus_id: corpus_id.clone(),
                        stale_claim_count: 1, // Indicates incomplete enrichment
                    });
                }
            }

            // The loop above can only ever see INSTALLED corpora, and
            // `installed_indexes()` drops any directory still flagged
            // `ingestion_in_progress` — which is exactly what an ingest that
            // died inside its enrichment phase leaves behind
            // (`<corpus_id>-partition-<node>/`, promotion runs only on `Ok`).
            // So the failure this checker most needs to describe was
            // structurally invisible to it: the corpus read as absent, and
            // the only trace was a WARN in the daemon log at the moment it
            // happened. `CorpusEngine::incomplete_ingests` is the other half
            // of that walk.
            //
            // Scoped to ingests whose recipe had asked for enrichment. An
            // interrupted plain ingest is a real problem, but it is not this
            // component's to report, and claiming it here would make the
            // enrichment report the machine's general ingest-failure log.
            for incomplete in self.engine.incomplete_ingests() {
                if !incomplete.enrichment_requested {
                    continue;
                }
                issues.push(HealthIssue::IncompleteIngestPartition {
                    corpus_id: incomplete.corpus_id,
                    path: incomplete.path.display().to_string(),
                    indexes_built: incomplete.indexes_built,
                });
            }

            let component = match indexes.len() {
                1 => Component::Enrichment(indexes[0].corpus_id.clone()),
                _ => Component::Enrichment("*".into()),
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
                HealthIssue::StaleEnrichment { corpus_id, .. } => {
                    Ok(RepairOutcome::NeedsUserDecision {
                        question: format!(
                            "Corpus `{corpus_id}` has an interrupted enrichment run. \
                             Resume field model enrichment?"
                        ),
                        options: vec![
                            sovereign_core::health::UserOption {
                                kind: RepairKind::RefreshEnrichment,
                                label: "Resume enrichment".into(),
                                description: "Resume from the last completed phase.".into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description:
                                    "Ignore — partial enrichment may affect search quality.".into(),
                            },
                        ],
                        consequence: "Resuming will use inference credits for remaining phases."
                            .into(),
                    })
                }
                HealthIssue::LowEnrichmentCoverage { corpus_id, .. } => {
                    Ok(RepairOutcome::NeedsUserDecision {
                        question: format!(
                            "Corpus `{corpus_id}` has no field model enrichment. \
                             Run enrichment now?"
                        ),
                        options: vec![
                            sovereign_core::health::UserOption {
                                kind: RepairKind::RefreshEnrichment,
                                label: "Run field model enrichment".into(),
                                description:
                                    "Build the field model (HDBSCAN clustering + LLM analysis)."
                                        .into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description: "Ignore — epistemic search will be unavailable."
                                    .into(),
                            },
                        ],
                        consequence: "Enrichment uses ~860 inference calls for SEP (~52 minutes)."
                            .into(),
                    })
                }
                // Explicit, not a fall-through: an incomplete ingest
                // partition is REPORTED here, not repaired. Deciding between
                // "resume this ingest" and "delete the directory" needs the
                // recipe and the source still to be available, which this
                // checker does not know; `sovereign corpus` owns that. Saying
                // so by name beats letting it drop into `_` where a future
                // reader cannot tell whether it was considered.
                HealthIssue::IncompleteIngestPartition { .. } => Err(Error::RepairNotSupported),
                _ => Err(Error::RepairNotSupported),
            }
        })
    }

    fn can_repair_autonomously(&self, _issue: &HealthIssue) -> bool {
        false
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn checker_exists() {
        // Basic compilation test — the checker struct compiles.
        // Integration tests require a real CorpusEngine.
    }
}
