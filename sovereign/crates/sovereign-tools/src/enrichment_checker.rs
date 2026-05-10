//! `EnrichmentChecker` — health-checks field model enrichment for every corpus.
//!
//! Checks for enrichment completeness based on the `field_skeleton.json`
//! artifact and the `_enrichment_checkpoint.json` resume state.

use std::sync::Arc;

use corpus_engine::CorpusEngine;
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
                if !info.enrichment_enabled {
                    continue;
                }
                let corpus_id = info.corpus_id.clone();

                if let Ok(index) = self.engine.open_index_for_corpus(&corpus_id).await {
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
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RepairOutcome>> + Send + '_>,
    > {
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
                                description: "Ignore — partial enrichment may affect search quality."
                                    .into(),
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
                                description: "Build the field model (HDBSCAN clustering + LLM analysis)."
                                    .into(),
                            },
                            sovereign_core::health::UserOption {
                                kind: RepairKind::Dismiss,
                                label: "Dismiss".into(),
                                description: "Ignore — epistemic search will be unavailable.".into(),
                            },
                        ],
                        consequence: "Enrichment uses ~860 inference calls for SEP (~52 minutes)."
                            .into(),
                    })
                }
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
